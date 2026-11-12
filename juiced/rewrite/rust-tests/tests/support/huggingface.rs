use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

pub const REVISION: &str = "89abcdef0123456789abcdef0123456789abcdef";

pub struct HubFile {
    pub path: String,
    pub bytes: Vec<u8>,
}
impl HubFile {
    pub fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.bytes))
    }
}

#[derive(Clone, Debug)]
pub struct Observed {
    pub path: String,
    pub authorization: Option<String>,
}

pub struct Hub {
    pub base: String,
    pub repos: Arc<BTreeMap<String, Vec<HubFile>>>,
    observed: Arc<Mutex<Vec<Observed>>>,
    task: JoinHandle<()>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn bytes(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| (index % 239) as u8 ^ seed)
        .collect()
}

async fn respond(socket: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
        body.len()
    );
    if socket.write_all(head.as_bytes()).await.is_ok() {
        let _ = socket.write_all(body).await;
    }
}

fn info(repo: &str, files: &[HubFile]) -> Value {
    let siblings: Vec<Value> = files
        .iter()
        .map(|file| json!({"rfilename": file.path, "size": file.bytes.len(), "lfs": {"sha256": file.sha256(), "size": file.bytes.len(), "pointerSize": 134}}))
        .chain([json!({"rfilename": ".gitattributes", "size": 1519})])
        .collect();
    json!({"id": repo, "sha": REVISION, "downloads": 41, "likes": 2, "pipeline_tag": "text-generation", "cardData": {"license": "apache-2.0"}, "siblings": siblings})
}

async fn serve(
    mut socket: TcpStream,
    repos: Arc<BTreeMap<String, Vec<HubFile>>>,
    observed: Arc<Mutex<Vec<Observed>>>,
) {
    let mut request = Vec::new();
    while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        if socket.read_exact(&mut byte).await.is_err() {
            return;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split(' ').next())
        .unwrap_or("")
        .to_owned();
    let authorization = text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    });
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    observed.lock().unwrap().push(Observed {
        path: path.to_owned(),
        authorization,
    });
    if path == "/api/models" {
        let items: Vec<Value> = repos
            .keys()
            .map(|repo| json!({"id": repo, "downloads": 41, "likes": 2, "pipeline_tag": "text-generation"}))
            .collect();
        let body = serde_json::to_vec(&items).unwrap();
        return respond(
            &mut socket,
            "200 OK",
            "Content-Type: application/json\r\n",
            &body,
        )
        .await;
    }
    let segments: Vec<&str> = path.trim_start_matches('/').splitn(6, '/').collect();
    match segments.as_slice() {
        ["api", "models", owner, name] | ["api", "models", owner, name, "revision", REVISION]
            if query.contains("blobs=true") =>
        {
            let repo = format!("{owner}/{name}");
            match repos.get(&repo) {
                Some(files) => {
                    let body = serde_json::to_vec(&info(&repo, files)).unwrap();
                    respond(
                        &mut socket,
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        &body,
                    )
                    .await
                }
                None => respond(&mut socket, "401 Unauthorized", "", b"{}").await,
            }
        }
        [owner, name, "resolve", REVISION, file] => {
            let repo = format!("{owner}/{name}");
            match repos
                .get(&repo)
                .and_then(|files| files.iter().find(|known| known.path == *file))
            {
                Some(found) => {
                    let headers = format!("ETag: \"{}\"\r\n", found.sha256());
                    respond(&mut socket, "200 OK", &headers, &found.bytes).await
                }
                None => respond(&mut socket, "404 Not Found", "", b"").await,
            }
        }
        _ => respond(&mut socket, "404 Not Found", "", b"").await,
    }
}

impl Hub {
    pub async fn start() -> Self {
        let file = |path: &str, seed, length| HubFile {
            path: path.to_owned(),
            bytes: bytes(seed, length),
        };
        Self::with_repos(BTreeMap::from([
            (
                "fixture/Process-GGUF".to_owned(),
                vec![
                    file("Process-Q4_K_M.gguf", 1, 24_000),
                    file("Process-Q8_0.gguf", 2, 25_000),
                    file("Process-IQ4_NL.gguf", 3, 26_000),
                    file("Process-MXFP4.gguf", 4, 27_000),
                ],
            ),
            (
                "fixture/Shard-GGUF".to_owned(),
                vec![
                    file("Shard-Q4_K_M-00001-of-00002.gguf", 5, 30_000),
                    file("Shard-Q4_K_M-00002-of-00002.gguf", 6, 20_000),
                ],
            ),
        ]))
        .await
    }

    pub async fn with_repos(repos: BTreeMap<String, Vec<HubFile>>) -> Self {
        let repos = Arc::new(repos);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let served = repos.clone();
        let recorded = observed.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        clients.spawn(serve(socket, served.clone(), recorded.clone()));
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Self {
            base,
            repos,
            observed,
            task,
        }
    }

    pub fn file(&self, repo: &str, path: &str) -> &HubFile {
        self.repos[repo]
            .iter()
            .find(|file| file.path == path)
            .unwrap()
    }

    pub fn observed(&self) -> Vec<Observed> {
        self.observed.lock().unwrap().clone()
    }
}
