use super::*;
use crate::container::{ContainerRequest, ResponseFormat};
use aileron_varlink::aileron_Inference::{
    Call_StreamRespondGuided2, Call_StreamResponse2, Call_StreamSubmitToolResultsGuided2,
    GenerationEvent, GenerationOptions, ReasoningCapabilities,
};

pub(super) trait EventCall {
    fn wants_events(&self) -> bool;
    fn emit(&mut self, event: GenerationEvent, more: bool) -> varlink::Result<()>;
    fn fail(&mut self, error: GenerationError) -> varlink::Result<()>;
}

macro_rules! event_call {
    ($call:ident) => {
        impl EventCall for dyn $call + '_ {
            fn wants_events(&self) -> bool {
                let call: &dyn $call = self;
                call.wants_more()
            }
            fn emit(&mut self, event: GenerationEvent, more: bool) -> varlink::Result<()> {
                let call: &mut dyn $call = self;
                call.set_continues(more);
                call.reply(event)
            }
            fn fail(&mut self, error: GenerationError) -> varlink::Result<()> {
                let call: &mut dyn $call = self;
                call.set_continues(false);
                reply_error(call, error)
            }
        }
    };
}
event_call!(Call_StreamResponse2);
event_call!(Call_StreamRespondGuided2);
event_call!(Call_StreamSubmitToolResultsGuided2);

pub(super) fn reply_error(
    call: &mut dyn VarlinkCallError,
    error: GenerationError,
) -> varlink::Result<()> {
    match error {
        GenerationError::SessionNotFound(id) => call.reply_session_not_found(id),
        GenerationError::ModelUnavailable(reason) => call.reply_model_unavailable(reason),
        GenerationError::InvalidOptions(reason) => call.reply_invalid_generation_options(reason),
        GenerationError::InvalidInput(reason) => call.reply_invalid_input(reason),
        GenerationError::Failed(reason) => reply_generation_failure(call, reason),
        GenerationError::Reply(error) => Err(error),
    }
}

pub(super) struct Request {
    pub session_id: String,
    pub input: String,
    pub media_paths: Vec<String>,
    pub fields: Option<Vec<GuidedField>>,
    pub tools: Vec<ToolDefinition>,
    pub results: Option<Vec<ToolResult>>,
    pub options: GenerationOptions,
}

fn event(kind: &str) -> GenerationEvent {
    GenerationEvent {
        kind: kind.into(),
        text: None,
        snapshot_json: None,
        tool_calls: None,
        finish_reason: None,
        usage: None,
    }
}

pub(super) async fn capabilities(
    state: &SharedState,
    session_id: &str,
) -> Result<ReasoningCapabilities, GenerationError> {
    let resolved = resolve_session_runtime(state, session_id, ensure_language_generation_use_case)
        .await
        .map_err(GenerationError::from)?;
    with_locked_container(
        "GetReasoningCapabilities",
        state,
        session_id,
        resolved,
        RequestExecutionMode::Interactive,
        GenerationError::Failed,
        |container, _, _| {
            let value = container
                .runtime_metadata
                .as_ref()
                .and_then(|v| v.get("reasoning"))
                .cloned()
                .unwrap_or_else(
                    || json!({"thinking_modes":[],"reasoning_efforts":[],"reasoning_output":false}),
                );
            serde_json::from_value(value).map_err(|e| GenerationError::Failed(e.to_string()))
        },
    )
    .await
}

pub(super) async fn serve<C: EventCall + ?Sized>(
    state: &SharedState,
    call: &mut C,
    request: Request,
) -> varlink::Result<()> {
    let wants_events = call.wants_events();
    if !wants_events && request.options.include_reasoning == Some(true) {
        return call.fail(GenerationError::InvalidOptions(
            "reasoning output requires a streaming Varlink call".into(),
        ));
    }
    let mut aggregate = event("completed");
    let result = generate(state, request, |incoming| {
        let done = incoming.kind == "completed";
        if wants_events {
            return call.emit(incoming, !done);
        }
        if incoming.kind == "answer" {
            aggregate
                .text
                .get_or_insert_with(String::new)
                .push_str(incoming.text.as_deref().unwrap_or_default());
        }
        if incoming.snapshot_json.is_some() {
            aggregate.snapshot_json = incoming.snapshot_json;
        }
        if incoming.tool_calls.is_some() {
            aggregate.tool_calls = incoming.tool_calls;
        }
        if done {
            aggregate.finish_reason = incoming.finish_reason;
            aggregate.usage = incoming.usage;
            call.emit(aggregate.clone(), false)?;
        }
        Ok(())
    })
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => call.fail(error),
    }
}

async fn generate(
    state: &SharedState,
    request: Request,
    mut emit: impl FnMut(GenerationEvent) -> varlink::Result<()>,
) -> Result<(), GenerationError> {
    let options = request.options;
    let (max_tokens, mode) = validate_token_options(
        options.maximum_response_tokens.unwrap_or(512),
        options.temperature.unwrap_or(0.0),
        options.execution_mode.as_deref().unwrap_or("interactive"),
    )
    .map_err(GenerationError::InvalidOptions)?;
    if options
        .thinking
        .as_deref()
        .is_some_and(|s| !matches!(s, "auto" | "on" | "off"))
        || (options.thinking.as_deref() == Some("off") && options.reasoning_effort.is_some())
    {
        return Err(GenerationError::InvalidOptions(
            "invalid or conflicting reasoning options".into(),
        ));
    }
    let resolved = resolve_session_runtime(
        state,
        &request.session_id,
        ensure_language_generation_use_case,
    )
    .await
    .map_err(GenerationError::from)?;
    let guided = request.fields.is_some();
    let mut media_budget = MediaBudget::default();
    let input = if guided {
        normalize_guided_input(&request.input, &request.media_paths, &mut media_budget)
    } else {
        normalize_stream_input(&request.input, &request.media_paths, &mut media_budget).map(Some)
    }
    .map_err(GenerationError::InvalidInput)?;
    let prompt = input
        .as_deref()
        .map(render_text_prompt)
        .unwrap_or(request.input);
    let schema = request
        .fields
        .as_ref()
        .map(|fields| guided_fields_schema(fields))
        .transpose()
        .map_err(GenerationError::Failed)?;
    let conversation = state
        .0
        .lock()
        .await
        .sessions
        .get(&request.session_id)
        .ok_or_else(|| GenerationError::SessionNotFound(request.session_id.clone()))?
        .tools
        .clone();
    with_locked_container(
        "Generate2",
        state,
        &request.session_id,
        resolved.clone(),
        mode,
        GenerationError::Failed,
        |container, _, _| {
            let caps = container
                .runtime_metadata
                .as_ref()
                .and_then(|value| value.get("reasoning"));
            let supports = |key: &str, choice: &str| {
                caps.and_then(|caps| caps[key].as_array())
                    .is_some_and(|values| values.iter().any(|value| value == choice))
            };
            if options
                .thinking
                .as_deref()
                .is_some_and(|mode| mode != "auto" && !supports("thinking_modes", mode))
                || options
                    .reasoning_effort
                    .as_deref()
                    .is_some_and(|effort| !supports("reasoning_efforts", effort))
                || (options.include_reasoning == Some(true)
                    && !caps.is_some_and(|caps| caps["reasoning_output"] == true))
            {
                return Err(GenerationError::InvalidOptions(
                    "requested reasoning controls are unsupported by this runtime/model".into(),
                ));
            }
            let mut runtime = ContainerRequest::new(
                Uuid::new_v4().to_string(),
                if guided {
                    "generate_structured_stream"
                } else {
                    "generate"
                },
            );
            runtime.system = Some(apply_translation_hints(
                &resolved.use_case,
                resolved.instructions.clone(),
                &ResponseOptions {
                    maximum_response_tokens: i64::from(max_tokens),
                    temperature: options.temperature.unwrap_or(0.0),
                    source_language_hint: options.source_language_hint.clone().unwrap_or_default(),
                    target_language_hint: options.target_language_hint.clone().unwrap_or_default(),
                    execution_mode: mode.as_str().into(),
                },
            ));
            runtime.prompt = Some(prompt.clone());
            runtime.input = input.clone();
            runtime.max_tokens = Some(max_tokens);
            runtime.temperature = options.temperature;
            runtime.thinking = options.thinking.clone();
            runtime.reasoning_effort = options.reasoning_effort.clone();
            runtime.include_reasoning = options.include_reasoning;
            runtime.execution_mode = Some(mode.as_str().into());
            runtime.response_format = schema.clone().map(|schema| ResponseFormat {
                r#type: "json_schema".into(),
                schema,
            });
            runtime.tools = Some(
                request
                    .tools
                    .into_iter()
                    .map(varlink_tool_definition)
                    .collect(),
            );
            let (epoch, context) = if let Some(results) = request.results {
                let results: Vec<_> = results.into_iter().map(varlink_tool_result).collect();
                let prepared = conversation
                    .lock()
                    .unwrap()
                    .continue_with(&results, &prompt, input.as_deref())
                    .map_err(|e| GenerationError::InvalidInput(e.to_string()))?;
                runtime.tool_results = Some(results);
                prepared
            } else if guided {
                conversation
                    .lock()
                    .unwrap()
                    .start(&prompt, input.as_deref())
            } else {
                (0, Value::Null)
            };
            if guided {
                runtime.tool_context = Some(context.clone());
            }
            let mut reply_error = None;
            let result = container.stream_generation(&runtime, |value| {
                RequestCancellation::for_epoch(state, &request.session_id, resolved.request_epoch)
                    .ensure_not_cancelled()
                    .map_err(anyhow::Error::msg)?;
                let mut events = Vec::new();
                if let Some(text) = value["token"].as_str() {
                    let mut e = event("answer");
                    e.text = Some(text.into());
                    events.push(e);
                }
                if options.include_reasoning == Some(true)
                    && let Some(text) = value["reasoning"].as_str()
                {
                    let mut e = event("reasoning");
                    e.text = Some(text.into());
                    events.push(e);
                }
                if let Some(snapshot) = value["snapshot"]
                    .as_str()
                    .or_else(|| value["result"].as_str())
                {
                    if let Some(schema) = &schema {
                        crate::container::validate_json_schema(snapshot, schema)?;
                    }
                    let mut e = event("snapshot");
                    e.snapshot_json = Some(snapshot.into());
                    events.push(e);
                }
                if let Some(calls) = value.get("tool_calls") {
                    let calls: Vec<crate::container::ToolCall> =
                        serde_json::from_value(calls.clone())?;
                    conversation
                        .lock()
                        .unwrap()
                        .remember(epoch, &context, &calls)?;
                    let mut e = event("tool_calls");
                    e.tool_calls = Some(varlink_tool_calls(calls));
                    events.push(e);
                }
                if value["done"] == true {
                    if guided && value.get("tool_calls").is_none() {
                        conversation.lock().unwrap().complete(epoch)?;
                    }
                    let mut e = event("completed");
                    e.finish_reason = value["finish_reason"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.get("tool_calls").map(|_| "tool_calls".to_owned()));
                    e.usage = value
                        .get("usage")
                        .filter(|v| !v.is_null())
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()?;
                    events.push(e);
                }
                for event in events {
                    if let Err(error) = emit(event) {
                        reply_error = Some(error);
                        anyhow::bail!("generation reply failed");
                    }
                }
                Ok(())
            });
            if let Some(error) = reply_error {
                return Err(GenerationError::Reply(error));
            }
            result.map_err(|e| GenerationError::Failed(e.to_string()))
        },
    )
    .await
}
