use std::io::BufRead;

use anyhow::{Context, Result, bail, ensure};

const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// SSE framing over bytes. Decode UTF-8 only once a full event is available.
pub(super) fn read_events(
    mut input: impl BufRead,
    mut event: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut line = Vec::new();
    let mut data = Vec::new();
    let mut event_bytes = 0;
    loop {
        let available = input.fill_buf().context("read server stream")?;
        ensure!(!available.is_empty(), "server stream ended before [DONE]");
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        ensure!(
            event_bytes + count <= MAX_EVENT_BYTES,
            "server SSE event exceeds size limit"
        );
        event_bytes += count;
        line.extend_from_slice(&available[..count]);
        input.consume(count);
        if line.last() != Some(&b'\n') {
            continue;
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            if !data.is_empty() {
                data.pop(); // Last SSE data-field newline.
                let text = std::str::from_utf8(&data).context("invalid UTF-8 in server event")?;
                if text == "[DONE]" {
                    return Ok(());
                }
                event(text)?;
                data.clear();
            }
            event_bytes = 0;
        } else if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            data.extend_from_slice(value);
            data.push(b'\n');
        } else if line == b"data" {
            data.push(b'\n');
        } else if !line.starts_with(b":")
            && !line.starts_with(b"event:")
            && !line.starts_with(b"id:")
            && !line.starts_with(b"retry:")
        {
            bail!("unexpected field in server SSE stream");
        }
        line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn framing_handles_every_read_size_and_utf8_boundary() {
        let bytes = ": heartbeat\r\ndata: {\"text\":\"Grüße 🌍\"}\r\n\r\ndata: second\ndata: line\n\ndata: [DONE]\n\n".as_bytes();
        for size in 1..=bytes.len() {
            let mut events = Vec::new();
            read_events(BufReader::with_capacity(size, bytes), |s| {
                events.push(s.to_owned());
                Ok(())
            })
            .unwrap();
            assert_eq!(events, ["{\"text\":\"Grüße 🌍\"}", "second\nline"]);
        }
    }

    #[test]
    fn rejects_truncation_invalid_utf8_and_oversized_events() {
        for bytes in [
            b"data: partial".to_vec(),
            b"data: \xff\n\n".to_vec(),
            vec![b'x'; MAX_EVENT_BYTES + 1],
        ] {
            assert!(read_events(BufReader::new(bytes.as_slice()), |_| Ok(())).is_err());
        }
    }
}
