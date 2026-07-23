mod audio;
pub(crate) mod phrase_buffer;

pub(crate) use audio::{AudioFormat, PcmFanout, PcmSink, PwPlayback, WavSink};

use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Default)]
pub(crate) struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) trait PhraseSynthesizer {
    fn synthesize(
        &mut self,
        phrase: &str,
        cancel: &CancellationToken,
        receive: &mut dyn FnMut(AudioFormat, Arc<[u8]>) -> Result<()>,
    ) -> Result<()>;

    fn cancel(&mut self) -> Result<()> {
        Ok(())
    }
}

pub(crate) fn run_ordered_synthesis(
    phrases: std::sync::mpsc::Receiver<String>,
    mut synthesizer: impl PhraseSynthesizer,
    mut fanout: PcmFanout,
    cancel: CancellationToken,
    keep_partial: bool,
) -> Result<()> {
    let result = (|| {
        while let Ok(phrase) = phrases.recv() {
            if cancel.is_cancelled() {
                bail!("speech synthesis cancelled");
            }
            synthesizer.synthesize(&phrase, &cancel, &mut |format, pcm| {
                if cancel.is_cancelled() {
                    bail!("speech synthesis cancelled");
                }
                fanout.write_cancellable(format, pcm, &cancel)
            })?;
        }
        if cancel.is_cancelled() {
            bail!("speech synthesis cancelled");
        }
        fanout.finish()
    })();

    if let Err(error) = result {
        let synthesis_cleanup = synthesizer.cancel();
        let sink_cleanup = fanout.cancel(keep_partial);
        if let Err(cleanup_error) = synthesis_cleanup.and(sink_cleanup) {
            return Err(anyhow::anyhow!(
                "{error}; speech cleanup also failed: {cleanup_error}"
            ));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeSynthesizer {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl PhraseSynthesizer for FakeSynthesizer {
        fn synthesize(
            &mut self,
            phrase: &str,
            _cancel: &CancellationToken,
            receive: &mut dyn FnMut(AudioFormat, Arc<[u8]>) -> Result<()>,
        ) -> Result<()> {
            self.calls.lock().unwrap().push(phrase.to_string());
            receive(
                AudioFormat {
                    sample_rate: 24_000,
                    channels: 1,
                    sample_format: "s16le".into(),
                },
                Arc::from(phrase.as_bytes()),
            )
        }
    }

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

    #[test]
    fn synthesizes_one_phrase_at_a_time_in_submission_order() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pcm = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender.send("one ".into()).unwrap();
        sender.send("two ".into()).unwrap();
        drop(sender);
        run_ordered_synthesis(
            receiver,
            FakeSynthesizer {
                calls: Arc::clone(&calls),
            },
            PcmFanout::new(vec![Box::new(CollectSink(Arc::clone(&pcm)))], 1),
            CancellationToken::default(),
            false,
        )
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), ["one ", "two "]);
        assert_eq!(*pcm.lock().unwrap(), b"one two ");
    }
}
