use std::{
    env,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::{Handle, Signals},
};
use tempfile::TempDir;

use super::transport::Transport;

pub(super) struct Server {
    child: Arc<Mutex<Child>>,
    signals: Handle,
    signal_thread: Option<JoinHandle<()>>,
    logs: Vec<JoinHandle<()>>,
    _directory: TempDir,
    pub transport: Transport,
    pub has_vision: bool,
    pub model_name: String,
}

fn number(name: &str, default: i32, min: i32) -> Result<i32> {
    let value = match env::var(name) {
        Ok(value) => value.parse().with_context(|| format!("invalid {name}"))?,
        Err(env::VarError::NotPresent) => default,
        Err(err) => return Err(err.into()),
    };
    ensure!(value >= min, "{name} must be at least {min}");
    Ok(value)
}

fn stop(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn diagnostic(line: &str) -> Option<&str> {
    // The daemon recognizes *any* case-insensitive occurrence of this word.
    (!line.to_ascii_lowercase().contains("ready")).then_some(line)
}

fn drain(input: impl std::io::Read + Send + 'static) -> JoinHandle<()> {
    thread::spawn(move || {
        // Bound each diagnostic chunk even when the child omits newlines.
        let mut input = BufReader::new(input);
        let mut line = Vec::new();
        while let Ok(bytes) = input.fill_buf() {
            if bytes.is_empty() {
                break;
            }
            let n = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| i + 1)
                .min(8192 - line.len());
            line.extend_from_slice(&bytes[..n]);
            input.consume(n);
            if line.last() == Some(&b'\n') || line.len() == 8192 {
                if let Some(text) = diagnostic(&String::from_utf8_lossy(&line)) {
                    eprintln!("[llama-server] {}", text.trim_end());
                }
                line.clear();
            }
        }
    })
}

impl Server {
    pub fn start() -> Result<Self> {
        let model =
            PathBuf::from(env::var("MODEL_PATH").unwrap_or_else(|_| "/model/model.gguf".into()));
        let model = model.canonicalize().context("resolve model file")?;
        ensure!(
            model.starts_with("/model") && model.is_file(),
            "model must be a local file under /model"
        );
        let device = env::var("AILERON_DEVICE").unwrap_or_else(|_| "cpu".into());
        let ctx = number("N_CTX", 4096, 1)?;
        let threads = number("N_THREADS", crate::default_threads_for_device(&device), 1)?;
        let gpu = number("N_GPU_LAYERS", 0, -1)?;
        let projector =
            PathBuf::from(env::var("MMPROJ_PATH").unwrap_or_else(|_| "/model/mmproj.gguf".into()));
        let has_vision = projector.is_file();
        let directory = tempfile::Builder::new()
            .prefix("aileron-llama-")
            .tempdir_in("/tmp")?;
        let socket = directory.path().join("server.sock");
        let mut command = Command::new("/usr/local/bin/llama-server");
        command
            .args(["--model"])
            .arg(&model)
            .arg("--host")
            .arg(&socket)
            .args([
                "--parallel",
                "1",
                "--threads-http",
                "4",
                // These spellings work on both the native reference revision
                // and newer servers; older servers do not enable Jinja by default.
                "--no-webui",
                "--jinja",
                "--no-warmup",
                // The daemon owns resource sizing and GPU fallback. Server-side
                // fitting repeats model inspection and can change that policy.
                "--fit",
                "off",
                "--ctx-size",
                &ctx.to_string(),
                "--threads",
                &threads.to_string(),
                "--threads-batch",
                &threads.to_string(),
                "--n-gpu-layers",
                &gpu.to_string(),
                "--no-context-shift",
                "--embeddings",
                "--pooling",
                "mean",
                "--batch-size",
                &ctx.to_string(),
                "--ubatch-size",
                &ctx.to_string(),
            ]);
        if has_vision {
            let projector = projector.canonicalize()?;
            ensure!(
                projector.starts_with("/model"),
                "projector must be under /model"
            );
            command.arg("--mmproj").arg(projector);
            if gpu == 0 {
                command.arg("--no-mmproj-offload");
            }
        }
        // Only adapter-selected configuration may influence server listeners and loading.
        command.env_clear();
        for name in [
            "PATH",
            "LD_LIBRARY_PATH",
            "AILERON_DEVICE",
            "CUDA_VISIBLE_DEVICES",
            "HIP_VISIBLE_DEVICES",
            "ROCR_VISIBLE_DEVICES",
            "VK_ICD_FILENAMES",
            "VK_DRIVER_FILES",
            "XDG_CACHE_HOME",
        ] {
            if let Some(value) = env::var_os(name) {
                command.env(name, value);
            }
        }
        // Identity lookup is independent of loading. Some GGUFs put their name
        // after a large vocabulary; do not serialize that scan with server startup.
        thread::scope(|scope| {
            let identity = scope.spawn(|| super::gguf::model_name(&model).unwrap_or_default());
            let mut server = Self::spawn(command, directory, &socket, Duration::from_secs(300))?;
            server.has_vision = has_vision;
            server.model_name = identity
                .join()
                .map_err(|_| anyhow::anyhow!("model identity reader panicked"))?;
            Ok(server)
        })
    }

    fn spawn(
        mut command: Command,
        directory: TempDir,
        socket: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        let transport = Transport::new(socket)?;
        let mut signals = Signals::new([SIGTERM, SIGINT])?;
        let handle = signals.handle();
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start private llama-server")?;
        let logs = vec![
            drain(child.stdout.take().unwrap()),
            drain(child.stderr.take().unwrap()),
        ];
        let child = Arc::new(Mutex::new(child));
        let signal_child = child.clone();
        let signal_thread = thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                stop(&mut signal_child.lock().unwrap());
                // The adapter can be blocked on stdin or HTTP. Reap before exit.
                std::process::exit(128 + signal);
            }
        });
        let mut server = Self {
            child,
            signals: handle,
            signal_thread: Some(signal_thread),
            logs,
            _directory: directory,
            transport,
            has_vision: false,
            model_name: String::new(),
        };
        let deadline = Instant::now() + timeout;
        loop {
            server.ensure_running()?;
            if server.transport.healthy() {
                return Ok(server);
            }
            ensure!(Instant::now() < deadline, "llama-server startup timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn ensure_running(&mut self) -> Result<()> {
        if let Some(status) = self.child.lock().unwrap().try_wait()? {
            anyhow::bail!("llama-server exited: {status}");
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.signals.close();
        if let Some(thread) = self.signal_thread.take() {
            let _ = thread.join();
        }
        stop(&mut self.child.lock().unwrap());
        for thread in self.logs.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_logs_cannot_trigger_early_readiness() {
        for line in ["ready", "server READY", "already loaded"] {
            assert_eq!(diagnostic(line), None);
        }
        assert_eq!(diagnostic("loading tensors"), Some("loading tensors"));
    }

    #[test]
    fn early_child_exit_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("server.sock");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let error = Server::spawn(command, directory, &socket, Duration::from_secs(2))
            .err()
            .unwrap();
        assert!(error.to_string().contains("exited"), "{error}");
    }

    #[test]
    fn stop_reaps_child_that_ignores_term() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; echo initialized; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "initialized");
        stop(&mut child);
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn health_readiness_and_drop_reap_the_server() {
        use std::{io::Write, os::unix::net::UnixListener};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("server.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let health = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                input.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let server = Server::spawn(command, directory, &socket, Duration::from_secs(2)).unwrap();
        let child = server.child.clone();
        drop(server);
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
        health.join().unwrap();
    }

    #[test]
    fn readiness_has_a_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("server.sock");
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let error = Server::spawn(command, directory, &socket, Duration::from_millis(50))
            .err()
            .unwrap();
        assert!(error.to_string().contains("timed out"), "{error}");
    }

    // Run in a separate test process: the production signal handler exits after
    // reaping, and must never install an exiting handler in the parent test run.
    #[test]
    fn signal_worker() {
        let Some(parent) = env::var_os("AILERON_SIGNAL_TEST_DIRECTORY") else {
            return;
        };
        let directory = tempfile::tempdir_in(&parent).unwrap();
        let socket = directory.path().join("server.sock");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "signal-test"])
            .arg(Path::new(&parent).join("child.pid"));
        let _server = Server::spawn(command, directory, &socket, Duration::from_secs(30)).unwrap();
        panic!("fake child must not become healthy");
    }

    #[test]
    fn termination_during_startup_reaps_child() {
        let directory = tempfile::tempdir().unwrap();
        let mut worker = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "llama_server::process::tests::signal_worker",
                "--nocapture",
            ])
            .env("AILERON_SIGNAL_TEST_DIRECTORY", directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid_file = directory.path().join("child.pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        let child_pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                break pid;
            }
            if Instant::now() >= deadline {
                let _ = worker.kill();
                let _ = worker.wait();
                panic!("signal test child did not start");
            }
            thread::sleep(Duration::from_millis(10));
        };
        kill(Pid::from_raw(worker.id() as i32), Signal::SIGTERM).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = worker.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = worker.kill();
                let _ = worker.wait();
                let _ = kill(Pid::from_raw(child_pid), Signal::SIGKILL);
                panic!("adapter signal handler did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(128 + SIGTERM));
        assert_eq!(
            kill(Pid::from_raw(child_pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
    }
}
