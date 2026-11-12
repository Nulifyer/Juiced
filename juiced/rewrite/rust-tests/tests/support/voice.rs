use super::Server;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use juiced_service::wyoming::{Event, encode_event, read_event};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

pub struct Wyoming {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Wyoming {
    pub async fn connect(address: &str) -> Self {
        let (read, writer) = TcpStream::connect(address).await.unwrap().into_split();
        Self {
            reader: BufReader::new(read),
            writer,
        }
    }
    pub async fn send(&mut self, kind: &str, data: Value, payload: &[u8]) {
        let mut event = Event::new(kind, data);
        event.payload = payload.to_vec();
        self.writer.write_all(&encode_event(&event)).await.unwrap();
    }
    pub async fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
    }
    pub async fn next(&mut self, timeout: Duration) -> Option<Event> {
        tokio::time::timeout(timeout, read_event(&mut self.reader))
            .await
            .expect("Wyoming reply timed out")
            .ok()
    }
    pub async fn transcribe(
        &mut self,
        language: &str,
        rate: u32,
        width: u16,
        channels: u16,
        pcm: &[u8],
    ) -> Event {
        let format = json!({"rate": rate, "width": width, "channels": channels});
        self.send("transcribe", json!({"language": language}), &[])
            .await;
        self.send("audio-start", format.clone(), &[]).await;
        for chunk in pcm.chunks(usize::from(width * channels) * 1024) {
            self.send("audio-chunk", format.clone(), chunk).await;
        }
        self.send("audio-stop", json!({}), &[]).await;
        self.next(Duration::from_secs(60))
            .await
            .expect("transcription reply")
    }
    pub async fn synthesize(&mut self, data: Value) -> (Event, Vec<u8>, usize) {
        self.send("synthesize", data, &[]).await;
        let start = self
            .next(Duration::from_secs(60))
            .await
            .expect("synthesis reply");
        if start.kind != "audio-start" {
            return (start, Vec::new(), 0);
        }
        let mut pcm = Vec::new();
        let mut chunks = 0;
        loop {
            let event = self
                .next(Duration::from_secs(10))
                .await
                .expect("audio event");
            match event.kind.as_str() {
                "audio-chunk" => {
                    assert_eq!(event.data["rate"], start.data["rate"]);
                    assert_eq!(event.data["width"], 2);
                    assert_eq!(event.data["channels"], 1);
                    pcm.extend(event.payload);
                    chunks += 1;
                }
                "audio-stop" => return (start, pcm, chunks),
                other => panic!("unexpected Wyoming event {other}"),
            }
        }
    }
}

pub async fn wyoming(server: &Server, settings: Value) -> Value {
    let generation = server.get("juiced/wyoming").await["configuration_generation"].clone();
    let (status, view) = server
        .post(
            "juiced/wyoming",
            json!({"settings": settings, "expected_generation": generation}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");
    view
}

pub fn tone(rate: u32, ms: u32, amplitude: f64) -> Vec<i16> {
    (0..rate * ms / 1000)
        .map(|index| {
            (amplitude
                * 32767.0
                * (2.0 * std::f64::consts::PI * 440.0 * f64::from(index) / f64::from(rate)).sin())
                as i16
        })
        .collect()
}

pub fn pcm(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn realtime(
    server: &Server,
    path: &str,
    key: Option<&str>,
    protocol: Option<&str>,
) -> Result<(Socket, Option<String>), tokio_tungstenite::tungstenite::Error> {
    let url = format!("{}{path}", server.base.replacen("http://", "ws://", 1));
    let mut request = url.into_client_request().unwrap();
    if let Some(key) = key {
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {key}")).unwrap(),
        );
    }
    if let Some(protocol) = protocol {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_str(protocol).unwrap(),
        );
    }
    let (socket, response) = connect_async(request).await?;
    let selected = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|value| value.to_str().unwrap().to_owned());
    Ok((socket, selected))
}

pub async fn send(socket: &mut Socket, event: Value) {
    socket.send(Message::text(event.to_string())).await.unwrap();
}

pub async fn append(socket: &mut Socket, samples: &[i16]) {
    for chunk in samples.chunks(2400) {
        let audio = base64::engine::general_purpose::STANDARD.encode(pcm(chunk));
        send(
            socket,
            json!({"type": "input_audio_buffer.append", "audio": audio}),
        )
        .await;
    }
}

pub async fn receive(socket: &mut Socket, timeout: Duration) -> Value {
    loop {
        let message = tokio::time::timeout(timeout, socket.next())
            .await
            .expect("Realtime event timed out")
            .expect("Realtime socket open")
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

pub async fn until(socket: &mut Socket, kind: &str, timeout: Duration) -> (Value, Vec<String>) {
    let mut seen = Vec::new();
    loop {
        let event = receive(socket, timeout).await;
        let event_kind = event["type"].as_str().unwrap().to_owned();
        seen.push(event_kind.clone());
        if event_kind == kind {
            return (event, seen);
        }
        assert_ne!(event_kind, "error", "{event}");
    }
}
