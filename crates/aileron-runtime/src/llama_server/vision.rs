use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::{ContentPart, Request};

const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const DESCRIPTION: &str = "Describe this image clearly and concisely. Include visible objects, people, text, and relevant context.";
const OCR: &str = "Extract all text visible in this image exactly as written. Preserve the reading order and line breaks. Return only the transcribed text with no commentary. If there is no text, return an empty response.";
const DETECT: &str = "Identify the main visible objects in this image. Return only JSON matching the schema. Use normalized bounding boxes where x and y are the top-left corner and width and height are relative to the image size.";

pub(super) fn has_image(req: &Request) -> bool {
    req.image.is_some()
        || req.input.as_ref().is_some_and(|messages| {
            messages.iter().any(|message| {
                message
                    .content
                    .iter()
                    .any(|part| matches!(part, ContentPart::InputImage { .. }))
            })
        })
}

pub(super) fn image_part(image: &Value) -> Result<Value> {
    if let Some(text) = image.as_str() {
        return encoded_image_part(text);
    }
    let bytes = if let Some(array) = image.as_array() {
        ensure!(array.len() <= MAX_IMAGE_BYTES, "image exceeds size limit");
        array
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| {
                        anyhow::anyhow!("image byte array must contain integers from 0 to 255")
                    })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        anyhow::bail!("image must be a base64 string or byte array");
    };
    let mime = image_mime(&bytes)?;
    let mut url = format!("data:{mime};base64,");
    STANDARD.encode_string(&bytes, &mut url);
    Ok(url_part(url))
}

pub(super) fn encoded_image_part(text: &str) -> Result<Value> {
    use std::io::Read;
    ensure!(
        text.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4,
        "image exceeds size limit"
    );
    // Validate the whole encoding without retaining a second image-sized buffer
    // or re-encoding already canonical input from the daemon.
    let mut decoder = base64::read::DecoderReader::new(text.as_bytes(), &STANDARD);
    let mut header = Vec::with_capacity(8);
    (&mut decoder).take(8).read_to_end(&mut header)?;
    let mime = image_mime(&header)?;
    let remaining = std::io::copy(&mut decoder, &mut std::io::sink())?;
    ensure!(
        remaining + header.len() as u64 <= MAX_IMAGE_BYTES as u64,
        "image exceeds size limit"
    );
    Ok(url_part(format!("data:{mime};base64,{text}")))
}

fn image_mime(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("image/jpeg")
    } else {
        anyhow::bail!("image must be PNG or JPEG")
    }
}

fn url_part(url: String) -> Value {
    let mut part = json!({"type":"image_url","image_url":{}});
    part["image_url"]["url"] = Value::String(url);
    part
}

pub(super) fn prompt(req: &Request) -> String {
    let (variable, default) = match req.request_type.as_str() {
        "ocr" => ("VISION_OCR_PROMPT", OCR),
        "detect" => ("VISION_DETECT_PROMPT", DETECT),
        _ => ("VISION_PROMPT", DESCRIPTION),
    };
    req.prompt
        .as_ref()
        .filter(|s| !s.is_empty())
        .cloned()
        .or_else(|| std::env::var(variable).ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| default.to_owned())
}

pub(super) fn detection_schema() -> Value {
    let number = json!({"type":"number", "minimum":0.0, "maximum":1.0});
    json!({"type":"object", "required":["detections"], "additionalProperties":false,
        "properties":{"detections":{"type":"array", "items":{
            "type":"object", "required":["label","confidence","x","y","width","height"],
            "additionalProperties":false, "properties":{"label":{"type":"string"},
                "confidence":number,"x":number,"y":number,"width":number,"height":number}}}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_image_representations_and_never_external_urls() {
        let bytes = b"\x89PNG\r\n\x1a\n";
        assert_eq!(
            image_part(&json!(bytes.to_vec())).unwrap(),
            image_part(&json!(STANDARD.encode(bytes))).unwrap()
        );
        for bad in [
            json!("https://example.com/image.png"),
            json!([256]),
            json!([1.5]),
            json!([]),
        ] {
            assert!(image_part(&bad).is_err());
        }
    }
}
