use std::{io::Read, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
    redirect::Policy,
};
use serde_json::Value;

pub(super) struct Transport {
    client: Client,
}

impl Transport {
    pub fn new(socket: &Path) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .unix_socket(socket.to_owned())
                .no_proxy()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(2))
                // Generation can take arbitrarily long; the daemon owns cancellation.
                .timeout(None)
                .build()
                .context("create private server client")?,
        })
    }

    pub fn healthy(&self) -> bool {
        self.client
            .get("http://localhost/health")
            .timeout(Duration::from_millis(500))
            .send()
            .is_ok_and(|response| response.status() == StatusCode::OK)
    }

    pub fn generate(&self, request: &Value) -> Result<Response> {
        let response = self
            .client
            .post("http://localhost/v1/chat/completions")
            .json(request)
            .send()
            .context("request private llama-server")?;
        let status = response.status();
        if !status.is_success() {
            let mut detail = String::new();
            response
                .take(4096)
                .read_to_string(&mut detail)
                .context("read server error")?;
            if let Ok(value) = serde_json::from_str::<Value>(&detail) {
                let max_tokens = request["max_tokens"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok());
                return Err(super::server_error(&value, max_tokens));
            }
            bail!("llama-server HTTP {status}: {detail}");
        }
        anyhow::ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(';').next() == Some("text/event-stream")),
            "server response is not an SSE stream"
        );
        Ok(response)
    }

    pub fn post_json(&self, route: &str, body: &Value) -> Result<Value> {
        let response = self
            .client
            .post(format!("http://localhost{route}"))
            .json(body)
            .send()?;
        let success = response.status().is_success();
        let mut bytes = Vec::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "server JSON exceeds size limit"
        );
        let value: Value = serde_json::from_slice(&bytes)?;
        if !success {
            return Err(super::server_error(
                &value,
                body["max_tokens"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok()),
            ));
        }
        Ok(value)
    }

    pub fn get_json(&self, route: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("http://localhost{route}"))
            .timeout(Duration::from_secs(5))
            .send()?
            .error_for_status()?;
        let mut bytes = Vec::new();
        response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= 1024 * 1024,
            "server properties exceed size limit"
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
    };

    #[test]
    fn unix_transport_decodes_chunked_body_and_refuses_redirects() {
        for kind in ["stream", "redirect", "context"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("server.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let worker = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if kind == "redirect" {
                    socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/escape\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else if kind == "context" {
                    let error = r#"{"error":{"type":"exceed_context_size_error","n_prompt_tokens":5000,"n_ctx":4096}}"#;
                    write!(socket, "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}", error.len()).unwrap();
                } else {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                    for byte in b"data: [DONE]\n\n" {
                        socket
                            .write_all(&[b'1', b'\r', b'\n', *byte, b'\r', b'\n'])
                            .unwrap();
                    }
                    socket.write_all(b"0\r\n\r\n").unwrap();
                }
            });
            let result = Transport::new(&path)
                .unwrap()
                .generate(&serde_json::json!({"max_tokens":64}));
            if kind == "redirect" {
                assert!(result.unwrap_err().to_string().contains("302"));
            } else if kind == "context" {
                let error = result.unwrap_err();
                let context = error
                    .downcast_ref::<crate::ContextWindowExceeded>()
                    .unwrap();
                assert_eq!(context.prompt_tokens, 5000);
                assert_eq!(context.max_tokens, Some(64));
            } else {
                assert_eq!(result.unwrap().text().unwrap(), "data: [DONE]\n\n");
            }
            worker.join().unwrap();
        }
    }
}
