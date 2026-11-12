use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::{JoinHandle, JoinSet},
};

pub struct Source {
    pub base: String,
    pub bytes: Arc<Vec<u8>>,
    pub ranges: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    gate: Option<Arc<Gate>>,
    task: JoinHandle<()>,
}
struct Gate {
    allowed: AtomicUsize,
    finished: AtomicBool,
    changed: Notify,
}
impl Gate {
    async fn wait(&self, end: usize, eof: bool) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.allowed.load(Ordering::Acquire) >= end
                    && (!eof || self.finished.load(Ordering::Acquire))
                {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("controlled artifact source gate exceeded its watchdog");
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve_source(
    mut socket: TcpStream,
    data: Arc<Vec<u8>>,
    ranges: Arc<AtomicUsize>,
    faults: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    gate: Option<Arc<Gate>>,
) {
    let mut request = Vec::new();
    while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if socket.read_exact(&mut byte).await.is_err() {
            return;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request);
    requests.fetch_add(1, Ordering::AcqRel);
    let offset = text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("range: bytes=")
                .and_then(|range| range.split('-').next())
                .and_then(|start| start.parse::<usize>().ok())
        })
        .unwrap_or(0)
        .min(data.len());
    if offset > 0 {
        ranges.fetch_add(1, Ordering::AcqRel);
    }
    let fail = text.starts_with("GET /fail-once ") && faults.fetch_add(1, Ordering::AcqRel) == 0;
    let status = if offset == 0 {
        "200 OK"
    } else {
        "206 Partial Content"
    };
    let range = if offset == 0 {
        String::new()
    } else {
        format!(
            "Content-Range: bytes {}-{}/{}\r\n",
            offset,
            data.len() - 1,
            data.len()
        )
    };
    let framing = if gate.is_some() {
        "Transfer-Encoding: chunked".to_owned()
    } else {
        format!("Content-Length: {}", data.len() - offset)
    };
    let header = format!(
        "HTTP/1.1 {status}\r\n{framing}\r\nETag: \"fixture-stable\"\r\nAccept-Ranges: bytes\r\n{range}Connection: close\r\n\r\n"
    );
    if socket.write_all(header.as_bytes()).await.is_err() {
        return;
    }
    for (index, chunk) in data[offset..].chunks(4096).enumerate() {
        if fail && index == 2 {
            return;
        }
        if let Some(gate) = &gate {
            gate.wait(offset + index * 4096 + chunk.len(), false).await;
            if socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
        if socket.write_all(chunk).await.is_err() {
            return;
        }
        if gate.is_some() {
            if socket.write_all(b"\r\n").await.is_err() {
                return;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }
    if let Some(gate) = &gate {
        gate.wait(data.len(), true).await;
        let _ = socket.write_all(b"0\r\n\r\n").await;
    }
}
impl Source {
    pub async fn start() -> Self {
        Self::with_bytes(Self::default_bytes()).await
    }
    pub async fn with_bytes(bytes: Vec<u8>) -> Self {
        Self::create(None, bytes).await
    }
    pub async fn gated(allowed_bytes: usize) -> Self {
        Self::gated_with_bytes(Self::default_bytes(), allowed_bytes).await
    }
    pub async fn gated_with_bytes(bytes: Vec<u8>, allowed_bytes: usize) -> Self {
        Self::create(
            Some(Arc::new(Gate {
                allowed: AtomicUsize::new(allowed_bytes),
                finished: AtomicBool::new(false),
                changed: Notify::new(),
            })),
            bytes,
        )
        .await
    }
    pub fn allow_through(&self, bytes: usize) {
        let gate = self.gate.as_ref().unwrap();
        gate.allowed.fetch_max(bytes, Ordering::AcqRel);
        gate.changed.notify_waiters();
    }
    pub fn finish(&self) {
        let gate = self.gate.as_ref().unwrap();
        gate.allowed.store(self.bytes.len(), Ordering::Release);
        gate.finished.store(true, Ordering::Release);
        gate.changed.notify_waiters();
    }
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::Acquire)
    }
    fn default_bytes() -> Vec<u8> {
        (0..512 * 1024).map(|index| (index % 251) as u8).collect()
    }
    async fn create(gate: Option<Arc<Gate>>, bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let bytes = Arc::new(bytes);
        let data = bytes.clone();
        let ranges = Arc::new(AtomicUsize::new(0));
        let observed = ranges.clone();
        let faults = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let observed_requests = requests.clone();
        let transfer_gate = gate.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let (socket,_)=accepted.unwrap();
                        clients.spawn(serve_source(socket,data.clone(),observed.clone(),faults.clone(),observed_requests.clone(),transfer_gate.clone()));
                    },
                    _=clients.join_next(), if !clients.is_empty()=>{}
                }
            }
        });
        Self {
            base,
            bytes,
            ranges,
            requests,
            gate,
            task,
        }
    }
    pub fn download(&self) -> Value {
        json!({"source_url":format!("{}/weights",self.base),"expected_sha256":format!("{:x}",Sha256::digest(&*self.bytes)),"expected_bytes":self.bytes.len(),"maximum_bytes":self.bytes.len()})
    }
}
