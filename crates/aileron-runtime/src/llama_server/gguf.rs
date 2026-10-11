//! Read only model identity metadata, without loading tensor data or a second engine.
use anyhow::{Result, ensure};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

struct MetadataReader {
    input: BufReader<File>,
    length: u64,
    position: u64,
}

impl MetadataReader {
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.input.read_exact(bytes)?;
        self.position += bytes.len() as u64;
        Ok(())
    }

    fn skip_bytes(&mut self, length: u64) -> Result<()> {
        let position = self
            .position
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("invalid GGUF offset"))?;
        ensure!(position <= self.length, "GGUF metadata exceeds file length");
        self.input.seek_relative(i64::try_from(length)?)?;
        self.position = position;
        Ok(())
    }
}

fn u32_le(file: &mut MetadataReader) -> Result<u32> {
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn u64_le(file: &mut MetadataReader) -> Result<u64> {
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn string(file: &mut MetadataReader) -> Result<String> {
    let length = u64_le(file)?;
    ensure!(length <= 1024 * 1024, "GGUF string exceeds metadata limit");
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)?;
    Ok(String::from_utf8(bytes)?)
}
fn scalar_size(kind: u32) -> Option<u64> {
    match kind {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

fn skip(file: &mut MetadataReader, kind: u32, depth: usize) -> Result<()> {
    ensure!(depth < 8, "GGUF metadata nesting exceeds limit");
    let length = match kind {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        8 => u64_le(file)?,
        9 => {
            let kind = u32_le(file)?;
            let count = u64_le(file)?;
            ensure!(count <= 1_000_000, "GGUF metadata array exceeds limit");
            if let Some(size) = scalar_size(kind) {
                return file.skip_bytes(count * size);
            }
            for _ in 0..count {
                skip(file, kind, depth + 1)?;
            }
            return Ok(());
        }
        _ => anyhow::bail!("unknown GGUF metadata type"),
    };
    file.skip_bytes(length)
}

pub(super) fn model_name(path: &Path) -> Result<String> {
    let input = File::open(path)?;
    let mut file = MetadataReader {
        length: input.metadata()?.len(),
        input: BufReader::with_capacity(128 * 1024, input),
        position: 0,
    };
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    ensure!(&magic == b"GGUF", "not a GGUF model");
    ensure!(
        matches!(u32_le(&mut file)?, 2 | 3),
        "unsupported GGUF version"
    );
    let _tensors = u64_le(&mut file)?;
    let count = u64_le(&mut file)?;
    ensure!(count <= 1_000_000, "GGUF metadata count exceeds limit");
    let mut architecture = String::new();
    for _ in 0..count {
        let key = string(&mut file)?;
        let kind = u32_le(&mut file)?;
        if key == "general.name" && kind == 8 {
            let name = string(&mut file)?;
            return Ok(if architecture.is_empty() {
                name
            } else {
                format!("{architecture} {name}")
            });
        }
        if key == "general.architecture" && kind == 8 {
            architecture = string(&mut file)?;
            continue;
        }
        skip(&mut file, kind, 0)?;
    }
    Ok(architecture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[ignore = "requires AILERON_GGUF_TEST_MODEL pointing at a local GGUF"]
    fn real_model_identity_lookup_is_fast() {
        let path = std::env::var("AILERON_GGUF_TEST_MODEL").unwrap();
        let start = std::time::Instant::now();
        let name = model_name(Path::new(&path)).unwrap();
        eprintln!("model identity {name:?}, lookup {:?}", start.elapsed());
        assert!(!name.is_empty());
        assert!(start.elapsed() < std::time::Duration::from_millis(250));
    }
    #[test]
    fn reads_identity_without_tensor_data_and_rejects_truncation() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut data = b"GGUF".to_vec();
        data.extend(3u32.to_le_bytes());
        data.extend(0u64.to_le_bytes());
        data.extend(1u64.to_le_bytes());
        data.extend(12u64.to_le_bytes());
        data.extend(b"general.name");
        data.extend(8u32.to_le_bytes());
        data.extend(9u64.to_le_bytes());
        data.extend(b"Gemma-4-B");
        file.write_all(&data).unwrap();
        assert_eq!(model_name(file.path()).unwrap(), "Gemma-4-B");
        file.as_file().set_len(30).unwrap();
        assert!(model_name(file.path()).is_err());
    }
}
