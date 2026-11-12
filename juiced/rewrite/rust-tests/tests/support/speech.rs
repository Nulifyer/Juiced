use super::Server;
use reqwest::{Method, Response};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, net::TcpStream};

pub enum Part<'a> {
    Text(&'a str, &'a str),
    File(&'a str, &'a str, &'a str, &'a [u8]),
}
pub fn multipart(parts: &[Part<'_>]) -> (String, Vec<u8>) {
    let boundary = "juiced-rust-speech-boundary-c783b402";
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::Text(name, value) => {
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                        .as_bytes(),
                );
            }
            Part::File(name, filename, mime, data) => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
                body.extend_from_slice(data);
                body.extend_from_slice(b"\r\n");
            }
        }
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}
pub fn audio() -> Vec<u8> {
    let data = b"private-audio-marker-do-not-log00";
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36u32 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}
pub async fn server(edit: impl FnOnce(&mut Value)) -> Server {
    Server::configured(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("whisper-cpp");
        model["operations"] = json!(["audio/transcriptions"]);
        model["max_inflight"] = json!(1);
        model["args"]
            .as_array_mut()
            .unwrap()
            .extend([json!("--host"), json!("127.0.0.1")]);
        edit(config);
    })
    .await
}
pub async fn send(server: &Server, path: &str, parts: &[Part<'_>], key: Option<&str>) -> Response {
    let (content_type, body) = multipart(parts);
    let mut request = server
        .client
        .request(Method::POST, format!("{}{path}", server.base))
        .header("content-type", content_type)
        .body(body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    request.send().await.unwrap()
}
pub async fn raw(server: &Server, parts: &[Part<'_>]) -> TcpStream {
    let (content_type, body) = multipart(parts);
    let key = server.read_key().unwrap();
    let mut socket = TcpStream::connect(server.base.trim_start_matches("http://"))
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/audio/transcriptions HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nAuthorization: Bearer {key}\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    socket
}
