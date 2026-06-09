/// Varlink handler for `aileron.Inference`.
use uuid::Uuid;

use crate::state::SharedState;
#[allow(unused_imports)]
// VarlinkCallError is a supertrait; its methods reach us via Call_* dyn objects
use aileron_varlink::aileron_Inference::{
    Call_CreateSession, Call_Describe, Call_EndSession, Call_Generate, Call_GenerateStructured,
    Call_Transcribe, VarlinkCallError, VarlinkInterface,
};

pub struct InferenceHandler {
    state: SharedState,
    rt: tokio::runtime::Handle,
}

impl InferenceHandler {
    pub fn new(state: SharedState, rt: tokio::runtime::Handle) -> Self {
        Self { state, rt }
    }
}

/// Convert an anyhow / string error into a varlink::Error.
fn io_err(_msg: impl std::fmt::Display) -> varlink::Error {
    varlink::Error::from(varlink::ErrorKind::Io(std::io::ErrorKind::Other))
}

impl VarlinkInterface for InferenceHandler {
    fn create_session(
        &self,
        call: &mut dyn Call_CreateSession,
        app_id: String,
        use_case: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            // Permission check — skipped entirely when allow_all is set.
            if !guard.config.allow_all {
                match guard.permissions.check(&app_id, &use_case) {
                    Some(true) => {}
                    Some(false) => return call.reply_permission_denied(app_id, use_case),
                    None => {
                        if guard.config.auto_grant {
                            tracing::info!(
                                "auto-granting {app_id} / {use_case} (AILERON_AUTO_GRANT)"
                            );
                            if let Err(e) =
                                guard.permissions.set(app_id.clone(), use_case.clone(), true)
                            {
                                tracing::warn!("failed to persist auto-grant: {e}");
                            }
                        } else {
                            return call.reply_permission_denied(app_id, use_case);
                        }
                    }
                }
            }

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => return call.reply_no_model_assigned(use_case),
            };

            // Warm up the container now so Generate doesn't block silently.
            // Status lines from the container's stderr are forwarded as
            // continues replies if the caller used `more`, otherwise logged.
            let wants_more = call.wants_more();
            let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
            guard
                .containers
                .get_or_spawn(&use_case, &image_ref, move |msg| {
                    let _ = status_tx.send(msg);
                })
                .map_err(io_err)?;

            // Drain any status messages collected during startup.
            if wants_more {
                call.set_continues(true);
                for msg in status_rx.try_iter() {
                    // Reuse session_id field as a status carrier — prefix with
                    // "status:" so the caller can distinguish it from the real ID.
                    call.reply(format!("status:{}", msg))?;
                }
                call.set_continues(false);
            }

            let session_id = Uuid::new_v4().to_string();
            let session = crate::state::Session {
                session_id: session_id.clone(),
                app_id,
                use_case,
                started_at: chrono::Utc::now(),
            };
            guard.sessions.insert(session_id.clone(), session);
            call.reply(session_id)
        })
    }

    fn generate(
        &self,
        call: &mut dyn Call_Generate,
        session_id: String,
        prompt: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => return call.reply_no_model_assigned(use_case),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            let wants_more = call.wants_more();
            let mut tokens: Vec<String> = Vec::new();

            container
                .generate(None, &prompt, 512, |token| {
                    tokens.push(token);
                })
                .map_err(io_err)?;

            if wants_more && tokens.len() > 1 {
                // Stream all but the last token with continues=true.
                call.set_continues(true);
                for token in &tokens[..tokens.len() - 1] {
                    call.reply(token.clone())?;
                }
                call.set_continues(false);
            }

            // Final (or only) token — sent without continues.
            let last = tokens.into_iter().last().unwrap_or_default();
            call.reply(last)
        })
    }

    fn transcribe(
        &self,
        call: &mut dyn Call_Transcribe,
        session_id: String,
        audio: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => return call.reply_no_model_assigned(use_case),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let audio_bytes = base64_decode(&audio).map_err(io_err)?;

            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            let text = container.transcribe(audio_bytes).map_err(io_err)?;

            call.reply(text)
        })
    }

    fn describe(
        &self,
        call: &mut dyn Call_Describe,
        session_id: String,
        image: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => return call.reply_no_model_assigned(use_case),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            let image_bytes = base64_decode(&image).map_err(io_err)?;

            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            let text = container.describe(image_bytes).map_err(io_err)?;

            call.reply(text)
        })
    }

    fn end_session(
        &self,
        call: &mut dyn Call_EndSession,
        session_id: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;
            if guard.sessions.remove(&session_id).is_none() {
                return call.reply_session_not_found(session_id);
            }
            call.reply()
        })
    }

    fn generate_structured(
        &self,
        call: &mut dyn Call_GenerateStructured,
        session_id: String,
        prompt: String,
        schema: String,
    ) -> varlink::Result<()> {
        self.rt.block_on(async {
            let mut guard = self.state.0.lock().await;

            let (app_id, use_case) = match guard.sessions.get(&session_id) {
                Some(s) => (s.app_id.clone(), s.use_case.clone()),
                None => return call.reply_session_not_found(session_id),
            };

            let image_ref = match guard.assignments.get(&use_case) {
                Some(r) => crate::hardware::resolve(r, guard.variant),
                None => return call.reply_no_model_assigned(use_case),
            };

            let _ = guard.permissions.touch(&app_id, &use_case);

            // Parse the caller-supplied schema string into a JSON value so it
            // can be forwarded to the container and used for validation.
            let schema_value: serde_json::Value = match serde_json::from_str(&schema) {
                Ok(v) => v,
                Err(e) => {
                    return call.reply_schema_validation_failed(format!("invalid schema JSON: {e}"))
                }
            };

            let container = guard
                .containers
                .get_or_spawn(&use_case, &image_ref, |_| {})
                .map_err(io_err)?;

            match container.generate_structured(None, &prompt, 1024, &schema_value) {
                Ok(result) => call.reply(result),
                Err(e) => {
                    let msg = e.to_string();
                    // Distinguish validation failures from I/O failures.
                    if msg.contains("not valid JSON")
                        || msg.contains("expected ")
                        || msg.contains("missing required")
                        || msg.contains("is not in enum")
                    {
                        call.reply_schema_validation_failed(msg)
                    } else {
                        Err(io_err(msg))
                    }
                }
            }
        })
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &b) in alphabet.iter().enumerate() {
        table[b as usize] = i as u8;
    }

    let clean: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;

    for b in clean {
        let v = table[b as usize];
        if v == 255 {
            return Err(format!("invalid base64 char: {}", b as char));
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
