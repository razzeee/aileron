/// Container lifecycle management.
///
/// One `podman` process is maintained per use-case. The process receives
/// newline-delimited JSON requests on stdin and emits newline-delimited JSON
/// response chunks on stdout.
///
/// Protocol:
///   Request:  {"id":"<uuid>","type":"generate","prompt":"...","max_tokens":512}
///   Response: {"id":"<uuid>","token":"Hello"}
///             {"id":"<uuid>","done":true}

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

/// A running container for a single use-case.
pub struct Container {
    pub image_ref: String,
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub last_used: std::time::Instant,
}

impl Container {
    /// Spawn `podman run --rm -i <image_ref>`.
    pub fn spawn(image_ref: &str) -> Result<Self> {
        info!("spawning container for {}", image_ref);
        let mut child = std::process::Command::new("podman")
            .args(["run", "--rm", "-i", image_ref])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn podman for {}", image_ref))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

        Ok(Self {
            image_ref: image_ref.to_string(),
            child,
            stdin,
            stdout,
            last_used: std::time::Instant::now(),
        })
    }

    /// Send a generate request and collect streamed token responses.
    /// The callback `on_token` is called for each token as it arrives.
    pub fn generate(
        &mut self,
        prompt: &str,
        max_tokens: u32,
        mut on_token: impl FnMut(String),
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "generate".to_string(),
            prompt: Some(prompt.to_string()),
            max_tokens: Some(max_tokens),
            audio: None,
            image: None,
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue; // stale from previous request
            }
            if let Some(token) = resp.token {
                on_token(token);
            }
            if resp.done.unwrap_or(false) {
                break;
            }
        }
        Ok(())
    }

    /// Send a transcribe request and return the full transcript.
    pub fn transcribe(&mut self, audio: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "transcribe".to_string(),
            prompt: None,
            max_tokens: None,
            audio: Some(audio),
            image: None,
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    /// Send a vision describe request and return the full description.
    pub fn describe(&mut self, image: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let req = ContainerRequest {
            id: id.clone(),
            r#type: "describe".to_string(),
            prompt: None,
            max_tokens: None,
            audio: None,
            image: Some(image),
        };
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.last_used = std::time::Instant::now();
        self.read_text_response(&id)
    }

    fn read_text_response(&mut self, id: &str) -> Result<String> {
        let mut result = String::new();
        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf)?;
            if n == 0 {
                bail!("container stdout closed unexpectedly");
            }
            let resp: ContainerResponse = serde_json::from_str(buf.trim())?;
            if resp.id != id {
                continue;
            }
            if let Some(token) = resp.token {
                result.push_str(&token);
            }
            if resp.done.unwrap_or(false) {
                break;
            }
        }
        Ok(result)
    }
}

/// Pool of running containers, keyed by use-case.
pub struct ContainerPool {
    containers: HashMap<String, Container>,
    /// Idle timeout in seconds (default 300 = 5 min).
    idle_timeout_secs: u64,
}

impl ContainerPool {
    pub fn new() -> Self {
        Self {
            containers: HashMap::new(),
            idle_timeout_secs: 300,
        }
    }

    /// Get or spawn a container for a use-case + image ref pair.
    pub fn get_or_spawn(
        &mut self,
        use_case: &str,
        image_ref: &str,
    ) -> Result<&mut Container> {
        if !self.containers.contains_key(use_case) {
            let c = Container::spawn(image_ref)?;
            self.containers.insert(use_case.to_string(), c);
        }
        Ok(self.containers.get_mut(use_case).unwrap())
    }

    /// Kill and remove the container for a use-case.
    pub fn kill(&mut self, use_case: &str) {
        if self.containers.remove(use_case).is_some() {
            info!("terminated container for use-case {}", use_case);
        }
    }

    /// Kill all containers.
    pub fn kill_all(&mut self) {
        let keys: Vec<_> = self.containers.keys().cloned().collect();
        for k in keys {
            self.kill(&k);
        }
    }

    /// Evict containers that have been idle longer than `idle_timeout_secs`.
    pub fn evict_idle(&mut self) {
        let timeout = std::time::Duration::from_secs(self.idle_timeout_secs);
        let now = std::time::Instant::now();
        let idle: Vec<_> = self
            .containers
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_used) > timeout)
            .map(|(k, _)| k.clone())
            .collect();
        for k in idle {
            warn!("evicting idle container for use-case {}", k);
            self.containers.remove(&k);
        }
    }
}

// ── internal protocol types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct ContainerRequest {
    id: String,
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct ContainerResponse {
    id: String,
    token: Option<String>,
    done: Option<bool>,
}
