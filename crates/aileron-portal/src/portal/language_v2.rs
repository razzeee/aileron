use super::*;
use aileron_varlink::aileron_Inference::{
    GenerationEvent, GenerationOptions, VarlinkClientInterface,
};
use zbus::zvariant::{OwnedValue, Value};

pub(super) type Options = HashMap<String, OwnedValue>;

fn option<T: TryFrom<OwnedValue>>(options: &mut Options, key: &str) -> zbus::fdo::Result<Option<T>>
where
    T::Error: std::fmt::Display,
{
    options
        .remove(key)
        .map(|value| {
            T::try_from(value)
                .map_err(|error| zbus::fdo::Error::InvalidArgs(format!("{key}: {error}")))
        })
        .transpose()
}

fn parse_options(mut values: Options) -> zbus::fdo::Result<GenerationOptions> {
    values.remove("handle_token");
    let options = GenerationOptions {
        maximum_response_tokens: option(&mut values, "maximum_response_tokens")?,
        temperature: option(&mut values, "temperature")?,
        source_language_hint: option(&mut values, "source_language_hint")?,
        target_language_hint: option(&mut values, "target_language_hint")?,
        execution_mode: option(&mut values, "execution_mode")?,
        thinking: option(&mut values, "thinking")?,
        reasoning_effort: option(&mut values, "reasoning_effort")?,
        include_reasoning: option(&mut values, "include_reasoning")?,
    };
    if !values.is_empty() {
        return Err(zbus::fdo::Error::InvalidArgs(
            "unknown generation option".into(),
        ));
    }
    Ok(options)
}

fn owned(value: impl Into<Value<'static>>) -> OwnedValue {
    OwnedValue::try_from(value.into()).expect("generated metadata contains no file descriptors")
}

pub(super) struct Request {
    pub input: String,
    pub fds: Vec<OwnedFd>,
    pub fields: Option<Vec<GuidedFieldDbus>>,
    pub tools: Vec<ToolDefinitionDbus>,
    pub results: Option<Vec<ToolResultDbus>>,
    pub options: Options,
}

#[derive(Default)]
struct ForwardState {
    completed: bool,
    snapshot: Option<String>,
    tools: Option<Vec<ToolCallDbus>>,
}

async fn forward(
    event: GenerationEvent,
    emitter: &SignalEmitter<'_>,
    request: &OwnedObjectPath,
    session: &OwnedObjectPath,
    guided: bool,
    pending: &mut ForwardState,
) -> zbus::Result<()> {
    if pending.completed {
        return Err(zbus::Error::Failure(
            "event after generation completion".into(),
        ));
    }
    match event.kind.as_str() {
        "answer" => {
            LanguagePortalBackend::token_received(
                emitter,
                request,
                session,
                event.text.as_deref().unwrap_or_default(),
                false,
            )
            .await?
        }
        "reasoning" => {
            LanguagePortalBackend::reasoning_received(
                emitter,
                request,
                session,
                event.text.as_deref().unwrap_or_default(),
            )
            .await?
        }
        "snapshot" => {
            if let Some(previous) = pending
                .snapshot
                .replace(event.snapshot_json.unwrap_or_default())
            {
                LanguagePortalBackend::guided_snapshot_received(
                    emitter, request, session, &previous, false,
                )
                .await?;
            }
        }
        "tool_calls" => {
            let calls = event
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|call| ToolCallDbus {
                    id: call.id,
                    name: call.name,
                    arguments_json: call.arguments_json,
                })
                .collect::<Vec<_>>();
            pending.tools = Some(calls);
        }
        "completed" => {
            pending.completed = true;
            let mut metadata = Options::new();
            if let Some(reason) = event.finish_reason {
                metadata.insert("finish_reason".into(), owned(reason));
            }
            if let Some(usage) = event.usage {
                let mut values = Options::new();
                for (key, value) in [
                    ("prompt_tokens", usage.prompt_tokens),
                    ("completion_tokens", usage.completion_tokens),
                    ("total_tokens", usage.total_tokens),
                ] {
                    if let Some(value) = value {
                        values.insert(key.into(), value.into());
                    }
                }
                metadata.insert("usage".into(), values.into());
            }
            // Existing consumers still receive a terminal stream signal. The
            // frontend also carries the metadata in Request.Response results.
            if let Some(calls) = pending.tools.take() {
                LanguagePortalBackend::guided_tool_calls_received(
                    emitter, request, session, &calls, true,
                )
                .await?;
            } else if guided {
                let snapshot = pending.snapshot.take().ok_or_else(|| {
                    zbus::Error::Failure("guided completion has no snapshot".into())
                })?;
                LanguagePortalBackend::guided_snapshot_received(
                    emitter, request, session, &snapshot, true,
                )
                .await?;
            } else {
                LanguagePortalBackend::token_received(emitter, request, session, "", true).await?;
            }
            LanguagePortalBackend::generation_completed(emitter, request, session, metadata)
                .await?;
        }
        _ => return Err(zbus::Error::Failure("unknown generation event".into())),
    }
    Ok(())
}

pub(super) async fn stream(
    backend: &LanguagePortalBackend,
    conn: &zbus::Connection,
    emitter: &SignalEmitter<'_>,
    request_handle: OwnedObjectPath,
    session_handle: OwnedObjectPath,
    request: Request,
) -> zbus::fdo::Result<()> {
    let request_id = request_handle.as_str();
    let session_id = session_handle.as_str();
    let options = parse_options(request.options)?;
    let background = options.execution_mode.as_deref() == Some("background");
    begin_request(conn, &backend.state, request_id, Some(session_id)).await?;
    let result = async {
        let record = ensure_known_session(&backend.state, session_id, PortalInterface::Language)?;
        ensure_language_generation_session(&record)?;
        attach_request_daemon_session(&backend.state, request_id, &record.daemon_session_id)?;
        backend
            .emit_loading(&request_handle, &session_handle, emitter)
            .await?;
        ensure_request_active(&backend.state, request_id)?;
        let media_paths = request.fds.iter().map(fd_proc_path).collect();
        let guided = request.fields.is_some();
        let mut pending = ForwardState::default();
        macro_rules! forward_call {
            ($client:ident, $call:expr) => {{
                let mut replies = stream_replies(
                    backend.state.clone(),
                    request_id,
                    request.fds,
                    background,
                    move |mut $client| $call,
                )
                .await?;
                while let Some(reply) = replies.recv().await {
                    ensure_request_active(&backend.state, request_id)?;
                    let event = reply
                        .map_err(|e| map_request_error(&backend.state, request_id, e))?
                        .event;
                    forward(
                        event,
                        emitter,
                        &request_handle,
                        &session_handle,
                        guided,
                        &mut pending,
                    )
                    .await
                    .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
                }
            }};
        }
        if let Some(fields) = request.fields {
            let fields = fields
                .into_iter()
                .map(GuidedFieldDbus::into_varlink)
                .collect();
            let tools = request
                .tools
                .into_iter()
                .map(ToolDefinitionDbus::into_varlink)
                .collect();
            if let Some(results) = request.results {
                let results = results
                    .into_iter()
                    .map(ToolResultDbus::into_varlink)
                    .collect();
                forward_call!(
                    client,
                    client.stream_submit_tool_results_guided2(
                        record.daemon_session_id,
                        request.input,
                        media_paths,
                        results,
                        fields,
                        tools,
                        options
                    )
                );
            } else {
                forward_call!(
                    client,
                    client.stream_respond_guided2(
                        record.daemon_session_id,
                        request.input,
                        media_paths,
                        fields,
                        tools,
                        options
                    )
                );
            }
        } else {
            forward_call!(
                client,
                client.stream_response2(
                    record.daemon_session_id,
                    request.input,
                    media_paths,
                    options
                )
            );
        }
        if !pending.completed {
            return Err(zbus::fdo::Error::Failed(
                "generation ended without completion".into(),
            ));
        }
        Ok(())
    }
    .await;
    finish_request(conn, &backend.state, request_id).await;
    result
}

pub(super) async fn capabilities(
    backend: &LanguagePortalBackend,
    conn: &zbus::Connection,
    emitter: &SignalEmitter<'_>,
    request: OwnedObjectPath,
    session: OwnedObjectPath,
) -> zbus::fdo::Result<()> {
    begin_request(
        conn,
        &backend.state,
        request.as_str(),
        Some(session.as_str()),
    )
    .await?;
    let result = async {
        let record =
            ensure_known_session(&backend.state, session.as_str(), PortalInterface::Language)?;
        ensure_language_generation_session(&record)?;
        attach_request_daemon_session(&backend.state, request.as_str(), &record.daemon_session_id)?;
        backend.emit_loading(&request, &session, emitter).await?;
        let state = backend.state.clone();
        let request_id = request.to_string();
        let caps = blocking(
            &TRANSPORT_WORKERS,
            Some((&backend.state, request.as_str())),
            move || {
                let connection = connect_request_daemon(&state, &request_id)?;
                let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(connection);
                client
                    .get_reasoning_capabilities(record.daemon_session_id)
                    .call()
                    .map(|reply| reply.capabilities)
                    .map_err(|e| map_request_error(&state, &request_id, e))
            },
        )
        .await?;
        ensure_request_active(&backend.state, request.as_str())?;
        let values = Options::from([
            ("thinking_modes".into(), owned(caps.thinking_modes)),
            ("reasoning_efforts".into(), owned(caps.reasoning_efforts)),
            ("reasoning_output".into(), caps.reasoning_output.into()),
        ]);
        LanguagePortalBackend::generation_completed(emitter, &request, &session, values)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    .await;
    finish_request(conn, &backend.state, request.as_str()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_omitted_temperature_and_rejects_wrong_types() {
        let parsed = parse_options(Options::from([("thinking".into(), owned("on"))])).unwrap();
        assert!(parsed.temperature.is_none());
        assert_eq!(parsed.thinking.as_deref(), Some("on"));
        assert!(
            parse_options(Options::from([("include_reasoning".into(), owned("true"))])).is_err()
        );
    }
}
