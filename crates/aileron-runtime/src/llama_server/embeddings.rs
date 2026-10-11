use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::transport::Transport;
use crate::Request;

pub(super) fn embed(transport: &Transport, req: &Request) -> Result<Value> {
    let body = json!({"input":req.prompt.as_deref().unwrap_or_default(),
        "embd_normalize":-1,"encoding_format":"float"});
    let response = transport.post_json("/v1/embeddings", &body)?;
    let items = response["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing embedding data"))?;
    ensure!(
        items.len() == 1 && items[0]["index"] == 0,
        "unexpected embedding count or index"
    );
    let vector = items[0]["embedding"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing flat embedding"))?;
    ensure!(
        !vector.is_empty()
            && vector
                .iter()
                .all(|v| v.as_f64().is_some_and(f64::is_finite)),
        "invalid embedding values"
    );
    Ok(json!({"id":req.id,"embedding":vector,"done":true}))
}
