use super::CancellationToken;
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) const MAX_AUDIO_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_format: String,
}

impl AudioFormat {
    pub(crate) fn validate_chunk(&self, pcm: &[u8]) -> Result<()> {
        if self.sample_format != "s16le" {
            bail!("unsupported PCM format: {}", self.sample_format);
        }
        if !(8_000..=192_000).contains(&self.sample_rate) {
            bail!("unsupported sample rate: {}", self.sample_rate);
        }
        if !(1..=2).contains(&self.channels) {
            bail!("unsupported channel count: {}", self.channels);
        }
        if pcm.len() > MAX_AUDIO_CHUNK_BYTES {
            bail!("audio chunk exceeds {MAX_AUDIO_CHUNK_BYTES} bytes");
        }
        let frame_bytes = self.channels as usize * 2;
        if !pcm.len().is_multiple_of(frame_bytes) {
            bail!("audio chunk ends inside a PCM sample frame");
        }
        Ok(())
    }
}

pub(crate) trait PcmSink: Send + 'static {
    fn start(&mut self, format: &AudioFormat) -> Result<()>;
    fn write(&mut self, pcm: &[u8]) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
    fn cancel(&mut self, keep_partial: bool) -> Result<()>;
}

enum SinkMessage {
    Start(AudioFormat),
    Chunk(Arc<[u8]>),
    Finish,
}

struct SinkWorker {
    sender: mpsc::SyncSender<SinkMessage>,
    cancel_sender: mpsc::Sender<bool>,
    errors: mpsc::Receiver<anyhow::Error>,
    thread: Option<JoinHandle<()>>,
}

impl SinkWorker {
    fn spawn(mut sink: Box<dyn PcmSink>, capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let (cancel_sender, cancel_receiver) = mpsc::channel();
        let (error_sender, errors) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            loop {
                if let Ok(keep_partial) = cancel_receiver.try_recv() {
                    while receiver.try_recv().is_ok() {}
                    if let Err(error) = sink.cancel(keep_partial) {
                        let _ = error_sender.send(error);
                    }
                    break;
                }
                let message = match receiver.recv_timeout(Duration::from_millis(10)) {
                    Ok(message) => message,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if let Ok(keep_partial) = cancel_receiver.try_recv() {
                    while receiver.try_recv().is_ok() {}
                    if let Err(error) = sink.cancel(keep_partial) {
                        let _ = error_sender.send(error);
                    }
                    break;
                }
                let terminal = matches!(message, SinkMessage::Finish);
                let result = match message {
                    SinkMessage::Start(format) => sink.start(&format),
                    SinkMessage::Chunk(pcm) => sink.write(&pcm),
                    SinkMessage::Finish => sink.finish(),
                };
                if let Err(error) = result {
                    let _ = error_sender.send(error);
                    break;
                }
                if terminal {
                    break;
                }
            }
        });
        Self {
            sender,
            cancel_sender,
            errors,
            thread: Some(thread),
        }
    }

    fn send_cancellable(&self, mut message: SinkMessage, cancel: &CancellationToken) -> Result<()> {
        loop {
            if cancel.is_cancelled() {
                bail!("speech synthesis cancelled");
            }
            if let Ok(error) = self.errors.try_recv() {
                return Err(error);
            }
            match self.sender.try_send(message) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(returned)) => {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    bail!("PCM sink stopped unexpectedly");
                }
            }
        }
    }

    fn join(&mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("PCM sink thread panicked"))?;
        }
        match self.errors.try_recv() {
            Ok(error) => Err(error),
            Err(_) => Ok(()),
        }
    }

    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

pub(crate) struct PcmFanout {
    workers: Vec<SinkWorker>,
    format: Option<AudioFormat>,
}

impl PcmFanout {
    pub(crate) fn new(sinks: Vec<Box<dyn PcmSink>>, queue_chunks: usize) -> Self {
        assert!(queue_chunks > 0);
        Self {
            workers: sinks
                .into_iter()
                .map(|sink| SinkWorker::spawn(sink, queue_chunks))
                .collect(),
            format: None,
        }
    }

    pub(crate) fn write_cancellable(
        &mut self,
        format: AudioFormat,
        pcm: Arc<[u8]>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        format.validate_chunk(&pcm)?;
        match &self.format {
            Some(active) if active != &format => bail!("PCM metadata changed during synthesis"),
            None => {
                for worker in &self.workers {
                    let message = SinkMessage::Start(format.clone());
                    worker.send_cancellable(message, cancel)?;
                }
                self.format = Some(format);
            }
            _ => {}
        }
        for worker in &self.workers {
            let message = SinkMessage::Chunk(Arc::clone(&pcm));
            worker.send_cancellable(message, cancel)?;
        }
        Ok(())
    }

    pub(crate) fn finish_cancellable(
        &mut self,
        cancel: &CancellationToken,
        keep_partial: bool,
    ) -> Result<()> {
        for worker in &self.workers {
            if let Err(error) = worker.send_cancellable(SinkMessage::Finish, cancel) {
                let _ = self.cancel(keep_partial);
                return Err(error);
            }
        }
        loop {
            if cancel.is_cancelled() {
                let _ = self.cancel(keep_partial);
                bail!("speech synthesis cancelled");
            }
            if self.workers.iter().all(SinkWorker::is_finished) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for worker in &mut self.workers {
            worker.join()?;
        }
        Ok(())
    }

    pub(crate) fn cancel(&mut self, keep_partial: bool) -> Result<()> {
        for worker in &self.workers {
            let _ = worker.cancel_sender.send(keep_partial);
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(250);
        while self.workers.iter().any(|worker| !worker.is_finished())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        for worker in self
            .workers
            .iter_mut()
            .filter(|worker| worker.is_finished())
        {
            worker.join()?;
        }
        Ok(())
    }
}

pub(crate) struct PwPlayback {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl PwPlayback {
    pub(crate) fn new() -> Self {
        Self {
            child: None,
            stdin: None,
        }
    }
}

impl PcmSink for PwPlayback {
    fn start(&mut self, format: &AudioFormat) -> Result<()> {
        let mut child = Command::new("pw-play")
            .args([
                "--raw",
                "--rate",
                &format.sample_rate.to_string(),
                "--channels",
                &format.channels.to_string(),
                "--format",
                "s16",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start pw-play")?;
        self.stdin = child.stdin.take();
        self.child = Some(child);
        Ok(())
    }

    fn write(&mut self, pcm: &[u8]) -> Result<()> {
        self.stdin
            .as_mut()
            .context("pw-play stdin is unavailable")?
            .write_all(pcm)
            .context("could not stream PCM to pw-play")
    }

    fn finish(&mut self) -> Result<()> {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let status = child.wait()?;
            if !status.success() {
                bail!("pw-play exited with {status}");
            }
        }
        Ok(())
    }

    fn cancel(&mut self, _keep_partial: bool) -> Result<()> {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for PwPlayback {
    fn drop(&mut self) {
        let _ = self.cancel(false);
    }
}

pub(crate) struct WavSink {
    destination: PathBuf,
    temporary: PathBuf,
    file: Option<File>,
    format: Option<AudioFormat>,
    pcm_bytes: u64,
}

impl WavSink {
    pub(crate) fn new(destination: impl Into<PathBuf>) -> Self {
        let destination = destination.into();
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("speech.wav");
        let temporary = destination.with_file_name(format!(".{file_name}.{suffix}.partial"));
        Self {
            destination,
            temporary,
            file: None,
            format: None,
            pcm_bytes: 0,
        }
    }

    fn finalize(&mut self) -> Result<()> {
        let format = self.format.as_ref().context("WAV sink was not started")?;
        let file = self.file.as_mut().context("WAV temporary file is closed")?;
        write_wav_header(file, format, self.pcm_bytes)?;
        file.flush()?;
        file.sync_all()?;
        self.file.take();
        std::fs::rename(&self.temporary, &self.destination)
            .with_context(|| format!("could not publish WAV to {}", self.destination.display()))
    }
}

impl PcmSink for WavSink {
    fn start(&mut self, format: &AudioFormat) -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&self.temporary)
            .with_context(|| format!("could not create {}", self.temporary.display()))?;
        file.write_all(&[0; 44])?;
        self.file = Some(file);
        self.format = Some(format.clone());
        Ok(())
    }

    fn write(&mut self, pcm: &[u8]) -> Result<()> {
        self.file
            .as_mut()
            .context("WAV sink is not started")?
            .write_all(pcm)?;
        self.pcm_bytes = self
            .pcm_bytes
            .checked_add(pcm.len() as u64)
            .context("WAV data is too large")?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.finalize()
    }

    fn cancel(&mut self, keep_partial: bool) -> Result<()> {
        if keep_partial && self.pcm_bytes > 0 {
            self.finalize()
        } else {
            self.file.take();
            match std::fs::remove_file(&self.temporary) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.temporary);
    }
}

fn write_wav_header(file: &mut File, format: &AudioFormat, pcm_bytes: u64) -> Result<()> {
    let data_len = u32::try_from(pcm_bytes).context("WAV data exceeds RIFF size limit")?;
    let byte_rate = format
        .sample_rate
        .checked_mul(format.channels)
        .and_then(|rate| rate.checked_mul(2))
        .context("invalid WAV byte rate")?;
    let block_align = u16::try_from(format.channels * 2)?;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(
        &36_u32
            .checked_add(data_len)
            .context("WAV data exceeds RIFF size limit")?
            .to_le_bytes(),
    );
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&u16::try_from(format.channels)?.to_le_bytes());
    header.extend_from_slice(&format.sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CollectSink(Arc<Mutex<Vec<u8>>>);

    impl PcmSink for CollectSink {
        fn start(&mut self, _format: &AudioFormat) -> Result<()> {
            Ok(())
        }
        fn write(&mut self, pcm: &[u8]) -> Result<()> {
            self.0.lock().unwrap().extend_from_slice(pcm);
            Ok(())
        }
        fn finish(&mut self) -> Result<()> {
            Ok(())
        }
        fn cancel(&mut self, _keep_partial: bool) -> Result<()> {
            Ok(())
        }
    }

    struct SlowSink {
        pcm: Arc<Mutex<Vec<u8>>>,
        cancelled: Arc<Mutex<Option<bool>>>,
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl PcmSink for SlowSink {
        fn start(&mut self, _format: &AudioFormat) -> Result<()> {
            Ok(())
        }

        fn write(&mut self, pcm: &[u8]) -> Result<()> {
            self.pcm.lock().unwrap().extend_from_slice(pcm);
            let _ = self.started.send(());
            let _ = self.release.recv();
            Ok(())
        }

        fn finish(&mut self) -> Result<()> {
            Ok(())
        }

        fn cancel(&mut self, keep_partial: bool) -> Result<()> {
            *self.cancelled.lock().unwrap() = Some(keep_partial);
            Ok(())
        }
    }

    fn format() -> AudioFormat {
        AudioFormat {
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".into(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aileron-demo-{name}-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn fanout_delivers_byte_identical_pcm_in_order() {
        let first = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(Mutex::new(Vec::new()));
        let mut fanout = PcmFanout::new(
            vec![
                Box::new(CollectSink(Arc::clone(&first))),
                Box::new(CollectSink(Arc::clone(&second))),
            ],
            1,
        );
        let cancel = CancellationToken::default();
        fanout
            .write_cancellable(format(), Arc::from([1, 2, 3, 4]), &cancel)
            .unwrap();
        fanout
            .write_cancellable(format(), Arc::from([5, 6]), &cancel)
            .unwrap();
        fanout.finish_cancellable(&cancel, false).unwrap();
        assert_eq!(*first.lock().unwrap(), [1, 2, 3, 4, 5, 6]);
        assert_eq!(*first.lock().unwrap(), *second.lock().unwrap());
    }

    #[test]
    fn cancellation_bypasses_a_full_sink_queue_and_drops_queued_audio() {
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(Mutex::new(None));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut fanout = PcmFanout::new(
            vec![Box::new(SlowSink {
                pcm: Arc::clone(&pcm),
                cancelled: Arc::clone(&cancelled),
                started: started_tx,
                release: release_rx,
            })],
            1,
        );
        let cancel = CancellationToken::default();

        fanout
            .write_cancellable(format(), Arc::from([1, 2]), &cancel)
            .unwrap();
        started_rx.recv().unwrap();
        fanout
            .write_cancellable(format(), Arc::from([3, 4]), &cancel)
            .unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release_tx.send(()).unwrap();
        });

        fanout.cancel(true).unwrap();

        assert_eq!(*pcm.lock().unwrap(), [1, 2]);
        assert_eq!(*cancelled.lock().unwrap(), Some(true));
    }

    #[test]
    fn cancellation_interrupts_finalization_with_a_stalled_sink() {
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(Mutex::new(None));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut fanout = PcmFanout::new(
            vec![Box::new(SlowSink {
                pcm,
                cancelled,
                started: started_tx,
                release: release_rx,
            })],
            1,
        );
        let cancel = CancellationToken::default();
        fanout
            .write_cancellable(format(), Arc::from([1, 2]), &cancel)
            .unwrap();
        started_rx.recv().unwrap();
        let cancel_from_thread = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let started = std::time::Instant::now();
        let error = fanout
            .finish_cancellable(&cancel, false)
            .expect_err("cancellation should interrupt finalization");

        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(1));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn wav_has_final_header_and_exact_pcm() {
        let path = temp_path("complete");
        let mut sink = WavSink::new(&path);
        sink.start(&format()).unwrap();
        sink.write(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        sink.finish().unwrap();
        let wav = std::fs::read(&path).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 44);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancellation_removes_output_by_default_and_can_publish_partial() {
        let removed = temp_path("removed");
        let mut sink = WavSink::new(&removed);
        sink.start(&format()).unwrap();
        sink.write(&[1, 2]).unwrap();
        sink.cancel(false).unwrap();
        assert!(!removed.exists());

        let kept = temp_path("kept");
        let mut sink = WavSink::new(&kept);
        sink.start(&format()).unwrap();
        sink.write(&[3, 4]).unwrap();
        sink.cancel(true).unwrap();
        assert_eq!(&std::fs::read(&kept).unwrap()[44..], &[3, 4]);
        std::fs::remove_file(kept).unwrap();
    }
}
