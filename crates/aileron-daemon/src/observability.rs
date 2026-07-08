use std::time::Instant;

const OBSERVABILITY_TARGET: &str = "aileron_daemon::observability";

pub(crate) struct SessionFields<'a> {
    pub session_id: &'a str,
    pub app_id: &'a str,
    pub use_case: &'a str,
    pub profile_id: &'a str,
}

pub(crate) struct InferenceRequestFields<'a> {
    pub method: &'static str,
    pub session_id: &'a str,
    pub app_id: &'a str,
    pub use_case: &'a str,
    pub profile_id: &'a str,
    pub runtime_id: &'a str,
    pub candidate_count: usize,
}

pub(crate) struct FailureSummary {
    pub code: &'static str,
    pub reason_len: usize,
}

pub(crate) trait ObservabilityFailure {
    fn observability_summary(&self) -> FailureSummary;
}

pub(crate) fn log_session_created(fields: SessionFields<'_>) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "session_created",
        session_id = %fields.session_id,
        app_id = %fields.app_id,
        use_case = %fields.use_case,
        profile_id = %fields.profile_id,
        "session created"
    );
}

pub(crate) fn log_session_ended(fields: SessionFields<'_>) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "session_ended",
        session_id = %fields.session_id,
        app_id = %fields.app_id,
        use_case = %fields.use_case,
        profile_id = %fields.profile_id,
        "session ended"
    );
}

pub(crate) fn log_inference_request_started(fields: InferenceRequestFields<'_>) -> Instant {
    let started_at = Instant::now();
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "inference_request_started",
        method = fields.method,
        session_id = %fields.session_id,
        app_id = %fields.app_id,
        use_case = %fields.use_case,
        profile_id = %fields.profile_id,
        runtime_id = %fields.runtime_id,
        candidate_count = fields.candidate_count,
        "inference request started"
    );
    started_at
}

pub(crate) fn log_inference_request_succeeded(
    fields: InferenceRequestFields<'_>,
    started_at: Instant,
    container_source: &'static str,
) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "inference_request_succeeded",
        method = fields.method,
        session_id = %fields.session_id,
        app_id = %fields.app_id,
        use_case = %fields.use_case,
        profile_id = %fields.profile_id,
        runtime_id = %fields.runtime_id,
        container_source = container_source,
        duration_ms = elapsed_ms(started_at),
        "inference request succeeded"
    );
}

pub(crate) fn log_inference_request_failed(
    fields: InferenceRequestFields<'_>,
    started_at: Instant,
    container_source: &'static str,
    failure: FailureSummary,
) {
    tracing::warn!(
        target: OBSERVABILITY_TARGET,
        event = "inference_request_failed",
        method = fields.method,
        session_id = %fields.session_id,
        app_id = %fields.app_id,
        use_case = %fields.use_case,
        profile_id = %fields.profile_id,
        runtime_id = %fields.runtime_id,
        container_source = container_source,
        failure_code = failure.code,
        reason_bytes = failure.reason_len,
        duration_ms = elapsed_ms(started_at),
        "inference request failed"
    );
}

pub(crate) fn log_runtime_starting(
    runtime_id: &str,
    image_ref: &str,
    variant: &str,
    runtime_option_count: usize,
) -> Instant {
    let started_at = Instant::now();
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_starting",
        runtime_id = %runtime_id,
        image_ref = %image_ref,
        variant = %variant,
        runtime_option_count = runtime_option_count,
        "runtime starting"
    );
    started_at
}

pub(crate) fn log_runtime_status(runtime_id: &str, image_ref: &str, variant: &str, line: &str) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_status",
        runtime_id = %runtime_id,
        image_ref = %image_ref,
        variant = %variant,
        status_kind = %runtime_status_kind(line),
        status_bytes = line.len(),
        "runtime status"
    );
}

pub(crate) fn log_runtime_ready(
    runtime_id: &str,
    image_ref: &str,
    variant: &str,
    started_at: Instant,
) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_ready",
        runtime_id = %runtime_id,
        image_ref = %image_ref,
        variant = %variant,
        duration_ms = elapsed_ms(started_at),
        "runtime ready"
    );
}

pub(crate) fn log_runtime_replacing_image(profile_id: &str, image_ref: &str, variant: &str) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_replacing",
        profile_id = %profile_id,
        image_ref = %image_ref,
        variant = %variant,
        "runtime replacing existing container"
    );
}

pub(crate) fn log_runtime_replacing_candidates(
    profile_id: &str,
    runtime_id: &str,
    candidate_count: usize,
) {
    tracing::info!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_replacing",
        profile_id = %profile_id,
        runtime_id = %runtime_id,
        candidate_count = candidate_count,
        "runtime replacing existing container"
    );
}

pub(crate) fn log_runtime_start_failed(
    profile_id: &str,
    runtime_id: &str,
    image_ref: &str,
    variant: &str,
    n_gpu_layers: Option<&str>,
    reason: &str,
) {
    tracing::warn!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_start_failed",
        profile_id = %profile_id,
        runtime_id = %runtime_id,
        image_ref = %image_ref,
        variant = %variant,
        n_gpu_layers = %n_gpu_layers.unwrap_or(""),
        failure_code = %runtime_start_failure_code(reason),
        error_bytes = reason.len(),
        "runtime start failed"
    );
}

pub(crate) fn log_runtime_evicted_idle(profile_id: &str, idle_timeout_secs: u64) {
    tracing::warn!(
        target: OBSERVABILITY_TARGET,
        event = "runtime_evicted_idle",
        profile_id = %profile_id,
        idle_timeout_secs = idle_timeout_secs,
        "runtime evicted after idle timeout"
    );
}

pub(crate) fn log_context_window_exceeded(
    prompt_tokens: Option<u64>,
    max_tokens: Option<u64>,
    context_tokens: Option<u64>,
    operation: Option<&str>,
) {
    let prompt_tokens = prompt_tokens
        .map(|value| value.to_string())
        .unwrap_or_default();
    let max_tokens = max_tokens
        .map(|value| value.to_string())
        .unwrap_or_default();
    let context_tokens = context_tokens
        .map(|value| value.to_string())
        .unwrap_or_default();
    tracing::warn!(
        target: OBSERVABILITY_TARGET,
        event = "context_window_exceeded",
        prompt_tokens = %prompt_tokens,
        max_tokens = %max_tokens,
        context_tokens = %context_tokens,
        operation = %operation.unwrap_or(""),
        "runtime context window exceeded"
    );
}

pub(crate) fn container_source(spawned: bool) -> &'static str {
    if spawned { "cold" } else { "warm" }
}

pub(crate) fn runtime_error_code(reason: &str) -> Option<&str> {
    reason
        .strip_prefix("container returned error ")
        .and_then(|rest| rest.split_once(':'))
        .map(|(code, _)| code.trim())
}

pub(crate) fn inference_failure_code(reason: &str) -> &'static str {
    if let Some(code) = runtime_error_code(reason) {
        match code {
            "context_window_exceeded" => "context_window_exceeded",
            "unsupported_language" => "unsupported_language",
            "safety_refusal" => "safety_refusal",
            "request_cancelled" => "request_cancelled",
            "invalid_input" => "invalid_input",
            _ => "runtime_error",
        }
    } else if reason.contains("container stdout closed unexpectedly") {
        "runtime_stdout_closed"
    } else if reason.contains("container sent done without") {
        "runtime_protocol_missing_field"
    } else if reason.contains("model returned no guided snapshots") {
        "empty_guided_output"
    } else if reason.contains("model returned no output") {
        "empty_output"
    } else if reason.contains("failed to read media path") {
        "media_read_failed"
    } else if reason.contains("media path must not be empty") {
        "invalid_media_path"
    } else if reason.contains("container mutex poisoned") {
        "container_lock_failed"
    } else if reason.contains("container was terminated before request started") {
        "runtime_terminated"
    } else if reason.ends_with("; retry request") {
        "retry_request"
    } else {
        "generation_failed"
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn runtime_status_kind(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();
    if lower.contains("ready") {
        "ready"
    } else if lower.contains("error") || lower.contains("failed") || lower.contains("panic") {
        "error"
    } else if lower.contains("warn") {
        "warning"
    } else if lower.contains('%') || lower.contains("progress") {
        "progress"
    } else if lower.contains("load") || lower.contains("model") || lower.contains("init") {
        "loading"
    } else {
        "other"
    }
}

fn runtime_start_failure_code(reason: &str) -> &'static str {
    if reason.contains("request_cancelled") {
        "request_cancelled"
    } else if reason.contains("container exited before ready") {
        "exited_before_ready"
    } else if reason.contains("container stderr thread dropped before ready") {
        "stderr_thread_dropped"
    } else if reason.contains("failed to spawn crun") {
        "crun_spawn_failed"
    } else if reason.contains("No such file") || reason.contains("not found") {
        "missing_runtime_dependency"
    } else {
        "start_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_kind_classifies_status_without_returning_content() {
        assert_eq!(runtime_status_kind("ready"), "ready");
        assert_eq!(
            runtime_status_kind("loading model from /private/path"),
            "loading"
        );
        assert_eq!(runtime_status_kind("download progress 42%"), "progress");
        assert_eq!(
            runtime_status_kind("warning: using cpu fallback"),
            "warning"
        );
        assert_eq!(runtime_status_kind("failed to initialize backend"), "error");
        assert_eq!(runtime_status_kind("arbitrary diagnostic text"), "other");
    }

    #[test]
    fn runtime_start_failure_code_classifies_without_returning_reason() {
        assert_eq!(
            runtime_start_failure_code("container exited before ready: secret detail"),
            "exited_before_ready"
        );
        assert_eq!(
            runtime_start_failure_code("failed to spawn crun for image"),
            "crun_spawn_failed"
        );
        assert_eq!(
            runtime_start_failure_code("container returned error request_cancelled: closed"),
            "request_cancelled"
        );
        assert_eq!(
            runtime_start_failure_code("unexpected detail"),
            "start_failed"
        );
    }

    #[test]
    fn inference_failure_code_uses_stable_runtime_codes() {
        assert_eq!(
            inference_failure_code(
                "container returned error request_cancelled: session was closed"
            ),
            "request_cancelled"
        );
        assert_eq!(
            inference_failure_code("container returned error safety_refusal: refused detail"),
            "safety_refusal"
        );
        assert_eq!(
            inference_failure_code("container returned error vendor_private: detail"),
            "runtime_error"
        );
    }

    #[test]
    fn inference_failure_code_classifies_daemon_side_failures() {
        assert_eq!(
            inference_failure_code("container stdout closed unexpectedly"),
            "runtime_stdout_closed"
        );
        assert_eq!(
            inference_failure_code("container sent done without a result field"),
            "runtime_protocol_missing_field"
        );
        assert_eq!(
            inference_failure_code("failed to read media path /secret/file: denied"),
            "media_read_failed"
        );
        assert_eq!(
            inference_failure_code("model returned no output"),
            "empty_output"
        );
        assert_eq!(
            inference_failure_code("unexpected detail"),
            "generation_failed"
        );
    }
}
