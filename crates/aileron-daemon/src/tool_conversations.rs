//! Session-owned tool history. Runtime reuse never owns conversation identity.
use crate::container::{InputMessage, ToolCall, ToolResult};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct ToolConversation {
    epoch: u64,
    pending: Option<(Value, Vec<ToolCall>)>,
}

impl ToolConversation {
    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
    }

    pub fn start(&mut self, prompt: &str, input: Option<&[InputMessage]>) -> (u64, Value) {
        self.cancel();
        (
            self.epoch,
            json!({"prompt":prompt,"input":input,"rounds":[]}),
        )
    }

    pub fn continue_with(
        &self,
        results: &[ToolResult],
        prompt: &str,
        input: Option<&[InputMessage]>,
    ) -> Result<(u64, Value)> {
        let (context, calls) = self
            .pending
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session has no pending tool calls"))?;
        ensure!(
            results.len() == calls.len(),
            "all pending tool results must be supplied together"
        );
        let mut ids = HashSet::new();
        for result in results {
            ensure!(
                ids.insert(&result.id) && calls.iter().any(|call| call.id == result.id),
                "unknown or duplicate tool result id"
            );
            if !result.content_json.is_empty() {
                let _: Value = serde_json::from_str(&result.content_json)?;
            }
        }
        let mut context = context.clone();
        let changed_input = input.is_some() && context["input"] != serde_json::to_value(input)?;
        let followup = if (!prompt.is_empty() && context["prompt"] != prompt) || changed_input {
            json!({"prompt":prompt,"input":input})
        } else {
            Value::Null
        };
        context["rounds"]
            .as_array_mut()
            .unwrap()
            .push(json!({"calls":calls,"results":results,"followup":followup}));
        Self::check_size(&context)?;
        Ok((self.epoch, context))
    }

    pub fn remember(&mut self, epoch: u64, context: &Value, calls: &[ToolCall]) -> Result<()> {
        ensure!(epoch == self.epoch, "tool turn was cancelled or replaced");
        ensure!(
            !calls.is_empty() && calls.len() <= 32,
            "invalid tool call count"
        );
        let mut ids = HashSet::new();
        for call in calls {
            ensure!(
                !call.id.is_empty() && ids.insert(&call.id),
                "duplicate or empty tool call id"
            );
            ensure!(
                !context["rounds"]
                    .as_array()
                    .is_some_and(|rounds| rounds.iter().any(|round| round["calls"]
                        .as_array()
                        .is_some_and(|previous| previous
                            .iter()
                            .any(|previous| previous["id"] == call.id)))),
                "tool call id was already used in this conversation"
            );
        }
        Self::check_size(&json!({"context":context,"calls":calls}))?;
        self.pending = Some((context.clone(), calls.to_vec()));
        Ok(())
    }

    pub fn complete(&mut self, epoch: u64) -> Result<()> {
        ensure!(epoch == self.epoch, "tool turn was cancelled or replaced");
        self.pending = None;
        Ok(())
    }

    fn check_size(context: &Value) -> Result<()> {
        ensure!(
            serde_json::to_vec(context)?.len() <= 64 * 1024 * 1024,
            "tool conversation exceeds size limit"
        );
        ensure!(
            context["rounds"]
                .as_array()
                .is_none_or(|rounds| rounds.len() <= 32),
            "too many tool rounds"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_isolation_cancellation_and_successful_continuation() {
        let mut first = ToolConversation::default();
        let second = ToolConversation::default();
        let (epoch, context) = first.start("question", None);
        let calls = vec![ToolCall {
            reasoning: None,
            id: "call-1".into(),
            name: "lookup".into(),
            arguments_json: "{}".into(),
        }];
        let results = vec![ToolResult {
            id: "call-1".into(),
            content: "answer".into(),
            content_json: String::new(),
        }];
        first.remember(epoch, &context, &calls).unwrap();
        assert!(second.continue_with(&results, "question", None).is_err());
        let (_, history) = first.continue_with(&results, "question", None).unwrap();
        assert_eq!(history["rounds"][0]["calls"][0]["id"], "call-1");
        first.cancel();
        assert!(first.remember(epoch, &context, &calls).is_err());
        assert!(first.continue_with(&results, "question", None).is_err());
    }
}
