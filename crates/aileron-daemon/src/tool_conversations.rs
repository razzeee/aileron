//! Session-owned tool history. Runtime reuse never owns conversation identity.
use crate::container::{InputMessage, ToolCall, ToolResult};
use aileron_tool_conversation_core::Conversation;
pub use aileron_tool_conversation_core::TurnId;
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Debug, Default)]
pub struct ToolConversation {
    core: Conversation,
    pending: Option<(Value, Vec<ToolCall>)>,
}

impl ToolConversation {
    pub fn cancel(&mut self) {
        self.core.cancel();
        self.pending = None;
    }

    pub fn start(
        &mut self,
        prompt: &str,
        input: Option<&[InputMessage]>,
    ) -> Result<(TurnId, Value)> {
        self.pending = None;
        let turn = self
            .core
            .start()
            .ok_or_else(|| anyhow::anyhow!("tool turn IDs exhausted; create a new session"))?;
        Ok((turn, json!({"prompt":prompt,"input":input,"rounds":[]})))
    }

    pub fn continue_with(
        &mut self,
        results: &[ToolResult],
        prompt: &str,
        input: Option<&[InputMessage]>,
    ) -> Result<(TurnId, Value)> {
        let (context, calls) = self
            .pending
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session has no pending tool calls"))?;
        let ids = results.iter().map(|result| result.id.clone()).collect();
        ensure!(
            self.core.accepts_results(&ids),
            "results must match all pending call IDs exactly, without duplicates"
        );
        for result in results {
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
            .ok_or_else(|| anyhow::anyhow!("invalid tool conversation rounds"))?
            .push(json!({"calls":calls,"results":results,"followup":followup}));
        Self::check_size(&context)?;
        // Commit only after every fallible JSON/size check. A successful continuation
        // consumes the batch and allocates a fresh turn, under the caller's mutex.
        let turn = self
            .core
            .continue_with(&ids)
            .ok_or_else(|| anyhow::anyhow!("tool turn IDs exhausted; create a new session"))?;
        self.pending = None;
        Ok((turn, context))
    }

    pub fn remember(&mut self, turn: TurnId, context: &Value, calls: &[ToolCall]) -> Result<()> {
        for call in calls {
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
        let ids = calls.iter().map(|call| call.id.clone()).collect();
        ensure!(
            self.core.remember(turn, ids),
            "invalid tool calls or tool turn was cancelled, replaced, or already answered"
        );
        self.pending = Some((context.clone(), calls.to_vec()));
        Ok(())
    }

    pub fn complete(&mut self, turn: TurnId) -> Result<()> {
        ensure!(
            self.core.complete(turn),
            "tool turn was cancelled or replaced"
        );
        // The Running state has no pending payload; Waiting must retain it when
        // a runtime emits tool_calls and done in separate events.
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

    fn calls(ids: &[&str]) -> Vec<ToolCall> {
        ids.iter()
            .map(|id| ToolCall {
                reasoning: None,
                id: (*id).into(),
                name: "lookup".into(),
                arguments_json: "{}".into(),
            })
            .collect()
    }

    fn results(ids: &[&str]) -> Vec<ToolResult> {
        ids.iter()
            .map(|id| ToolResult {
                id: (*id).into(),
                content: "answer".into(),
                content_json: String::new(),
            })
            .collect()
    }

    #[test]
    fn stale_callbacks_cannot_overwrite_new_conversation_or_continuation() {
        let mut conversation = ToolConversation::default();
        let (old, old_context) = conversation.start("old", None).unwrap();
        let (new, context) = conversation.start("new", None).unwrap();
        conversation
            .remember(new, &context, &calls(&["a"]))
            .unwrap();
        assert!(
            conversation
                .remember(old, &old_context, &calls(&["stale"]))
                .is_err()
        );
        assert!(conversation.complete(old).is_err());
        let (next, history) = conversation
            .continue_with(&results(&["a"]), "new", None)
            .unwrap();
        assert_ne!(new, next);
        assert!(conversation.complete(new).is_err());
        assert!(
            conversation
                .remember(new, &context, &calls(&["stale"]))
                .is_err()
        );
        assert!(
            conversation
                .continue_with(&results(&["a"]), "new", None)
                .is_err()
        );
        conversation
            .remember(next, &history, &calls(&["b"]))
            .unwrap();
        assert!(
            conversation
                .continue_with(&results(&["b"]), "new", None)
                .is_ok()
        );
    }

    #[test]
    fn invalid_results_are_atomic_and_done_preserves_pending_calls() {
        let mut conversation = ToolConversation::default();
        let (turn, context) = conversation.start("question", None).unwrap();
        conversation
            .remember(turn, &context, &calls(&["a", "b"]))
            .unwrap();
        conversation.complete(turn).unwrap();
        for ids in [
            vec!["a"],
            vec!["a", "a"],
            vec!["a", "unknown"],
            vec!["a", "b", "c"],
        ] {
            assert!(
                conversation
                    .continue_with(&results(&ids), "question", None)
                    .is_err()
            );
        }
        let mut malformed = results(&["a", "b"]);
        malformed[0].content_json = "{".into();
        assert!(
            conversation
                .continue_with(&malformed, "question", None)
                .is_err()
        );
        let (next, history) = conversation
            .continue_with(&results(&["b", "a"]), "followup", None)
            .unwrap();
        assert_eq!(history["rounds"][0]["followup"]["prompt"], "followup");
        assert!(
            conversation
                .remember(next, &history, &calls(&["a"]))
                .is_err()
        );
        conversation
            .remember(next, &history, &calls(&["c"]))
            .unwrap();
    }

    #[test]
    fn cancelled_and_finished_turns_cannot_be_revived() {
        let mut conversation = ToolConversation::default();
        let (turn, context) = conversation.start("question", None).unwrap();
        conversation.cancel();
        assert!(conversation.complete(turn).is_err());
        assert!(
            conversation
                .remember(turn, &context, &calls(&["a"]))
                .is_err()
        );
        let (next, context) = conversation.start("next", None).unwrap();
        assert_ne!(turn, next);
        for ids in [vec![], vec![""], vec!["a", "a"], vec!["a"; 33]] {
            assert!(conversation.remember(next, &context, &calls(&ids)).is_err());
        }
        conversation.complete(next).unwrap();
        assert!(
            conversation
                .remember(next, &context, &calls(&["a"]))
                .is_err()
        );
    }

    #[test]
    fn session_isolation_cancellation_and_successful_continuation() {
        let mut first = ToolConversation::default();
        let mut second = ToolConversation::default();
        let (epoch, context) = first.start("question", None).unwrap();
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
