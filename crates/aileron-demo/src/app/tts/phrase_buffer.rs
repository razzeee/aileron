use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(crate) struct PhraseBufferConfig {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub soft_min_chars: usize,
}

impl Default for PhraseBufferConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(200),
            max_bytes: 512,
            soft_min_chars: 32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushReason {
    Sentence,
    SoftPunctuation,
    Timeout,
    MaximumSize,
    Final,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Phrase {
    pub text: String,
    pub reason: FlushReason,
}

pub(crate) struct PhraseBuffer {
    config: PhraseBufferConfig,
    text: String,
    oldest: Option<Instant>,
}

impl PhraseBuffer {
    pub(crate) fn new(config: PhraseBufferConfig) -> Self {
        assert!(
            config.max_bytes >= 4,
            "max phrase size must fit a UTF-8 code point"
        );
        Self {
            config,
            text: String::new(),
            oldest: None,
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.oldest.map(|oldest| oldest + self.config.timeout)
    }

    pub(crate) fn push(&mut self, fragment: &str, now: Instant) -> Vec<Phrase> {
        if !fragment.is_empty() {
            if self.oldest.is_none() && fragment.chars().any(|ch| !ch.is_whitespace()) {
                self.oldest = Some(now);
            }
            self.text.push_str(fragment);
        }
        self.flush_available(now, false)
    }

    pub(crate) fn flush_due(&mut self, now: Instant) -> Vec<Phrase> {
        self.flush_available(now, false)
    }

    pub(crate) fn finish(&mut self, now: Instant) -> Vec<Phrase> {
        self.flush_available(now, true)
    }

    fn flush_available(&mut self, now: Instant, final_flush: bool) -> Vec<Phrase> {
        let mut phrases = Vec::new();
        loop {
            let boundary = self
                .sentence_boundary(final_flush)
                .filter(|end| *end <= self.config.max_bytes)
                .map(|end| (end, FlushReason::Sentence))
                .or_else(|| {
                    self.soft_boundary()
                        .filter(|end| *end <= self.config.max_bytes)
                        .map(|end| (end, FlushReason::SoftPunctuation))
                })
                .or_else(|| {
                    self.maximum_boundary()
                        .map(|end| (end, FlushReason::MaximumSize))
                })
                .or_else(|| {
                    self.timeout_boundary(now)
                        .map(|end| (end, FlushReason::Timeout))
                })
                .or_else(|| final_flush.then_some((self.text.len(), FlushReason::Final)));

            let Some((end, reason)) = boundary else {
                break;
            };
            if let Some(phrase) = self.take_phrase(end, reason, now) {
                phrases.push(phrase);
            } else if self.text.trim().is_empty() {
                self.text.clear();
                self.oldest = None;
                break;
            } else {
                break;
            }
        }
        phrases
    }

    fn sentence_boundary(&self, stream_ending: bool) -> Option<usize> {
        let mut chars = self.text.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            if matches!(ch, '.' | '?' | '!')
                && (chars.peek().is_some_and(|(_, next)| next.is_whitespace())
                    || (stream_ending && chars.peek().is_none()))
            {
                return Some(index + ch.len_utf8());
            }
        }
        None
    }

    fn soft_boundary(&self) -> Option<usize> {
        let mut count = 0;
        for (index, ch) in self.text.char_indices() {
            count += 1;
            if count >= self.config.soft_min_chars && matches!(ch, ',' | ';' | ':') {
                return Some(index + ch.len_utf8());
            }
        }
        None
    }

    fn maximum_boundary(&self) -> Option<usize> {
        if self.text.len() < self.config.max_bytes {
            return None;
        }
        let hard_end = floor_char_boundary(&self.text, self.config.max_bytes);
        self.text[..hard_end]
            .char_indices()
            .filter(|(_, ch)| ch.is_whitespace())
            .map(|(index, _)| index)
            .rfind(|index| *index > 0)
            .or(Some(hard_end))
    }

    fn timeout_boundary(&self, now: Instant) -> Option<usize> {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.text
            .char_indices()
            .filter(|(_, ch)| ch.is_whitespace())
            .map(|(index, _)| index)
            .rfind(|index| *index > 0)
    }

    fn take_phrase(&mut self, end: usize, reason: FlushReason, now: Instant) -> Option<Phrase> {
        let remainder = self.text.split_off(end);
        let phrase = std::mem::replace(&mut self.text, remainder)
            .trim()
            .to_string();
        self.oldest = self
            .text
            .chars()
            .any(|ch| !ch.is_whitespace())
            .then_some(now);
        (!phrase.is_empty()).then_some(Phrase {
            text: phrase,
            reason,
        })
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> (PhraseBuffer, Instant) {
        (
            PhraseBuffer::new(PhraseBufferConfig {
                timeout: Duration::from_millis(200),
                max_bytes: 16,
                soft_min_chars: 8,
            }),
            Instant::now(),
        )
    }

    #[test]
    fn flushes_sentence_only_after_following_whitespace() {
        let (mut buffer, now) = buffer();
        assert!(buffer.push("Hello.", now).is_empty());
        assert_eq!(
            buffer.push(" Next", now),
            vec![Phrase {
                text: "Hello.".into(),
                reason: FlushReason::Sentence,
            }]
        );
    }

    #[test]
    fn flushes_soft_punctuation_when_phrase_is_large_enough() {
        let (mut buffer, now) = buffer();
        assert_eq!(
            buffer.push("1234567,", now),
            vec![Phrase {
                text: "1234567,".into(),
                reason: FlushReason::SoftPunctuation,
            }]
        );
    }

    #[test]
    fn exposes_a_controllable_deadline_and_flushes_at_a_word_boundary() {
        let (mut buffer, now) = buffer();
        buffer.push("hello world", now);
        assert_eq!(buffer.deadline(), Some(now + Duration::from_millis(200)));
        assert!(
            buffer
                .flush_due(now + Duration::from_millis(199))
                .is_empty()
        );
        assert_eq!(
            buffer.flush_due(now + Duration::from_millis(200))[0].text,
            "hello"
        );
    }

    #[test]
    fn maximum_size_never_splits_unicode() {
        let (mut buffer, now) = buffer();
        let phrases = buffer.push("rust 🦀 language", now);
        assert_eq!(phrases[0].text, "rust 🦀");
        assert!(phrases[0].text.is_char_boundary(phrases[0].text.len()));
        assert!(phrases[0].text.len() <= 16);
    }

    #[test]
    fn final_flush_emits_remaining_text_but_not_whitespace() {
        let (mut phrase_buffer, now) = buffer();
        phrase_buffer.push("最後の言葉", now);
        assert_eq!(phrase_buffer.finish(now)[0].text, "最後の言葉");

        let (mut empty, now) = buffer();
        empty.push("   ", now);
        assert!(empty.finish(now).is_empty());
    }
}
