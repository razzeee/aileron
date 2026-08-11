#![allow(non_camel_case_types, clippy::too_many_arguments)]

use serde::{Deserialize, Serialize};
use zlink::introspect::{CustomType, Type};

macro_rules! wire_struct {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, CustomType)]
        pub struct $name { $(pub $field: $ty),* }
    };
}

macro_rules! wire_reply {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
        pub struct $name { $(pub $field: $ty),* }
    };
}

wire_struct!(ModelAvailability {
    is_available: bool,
    code: String,
    reason: String
});
wire_struct!(ResponseOptions {
    maximum_response_tokens: i64,
    temperature: f64,
    source_language_hint: String,
    target_language_hint: String,
    execution_mode: String
});
wire_struct!(GuidedOptions {
    maximum_response_tokens: i64,
    temperature: f64,
    execution_mode: String
});
wire_struct!(EmbedOptions {
    execution_mode: String
});
wire_struct!(SpeechOptions {
    source_language_hint: String,
    execution_mode: String
});
wire_struct!(SynthesisOptions {
    voice_id: String,
    language_hint: String,
    execution_mode: String
});
wire_struct!(AudioChunk {
    audio_base64: String,
    sample_rate: i64,
    channels: i64,
    sample_format: String
});
wire_struct!(VisionOptions {
    execution_mode: String
});
wire_struct!(VisionPointPrompt {
    x: f64,
    y: f64,
    positive: bool
});
wire_struct!(VisionBoxPrompt {
    x: f64,
    y: f64,
    width: f64,
    height: f64
});
wire_struct!(VisionSegmentOptions { execution_mode: String, points: Vec<VisionPointPrompt>, boxes: Vec<VisionBoxPrompt> });
wire_struct!(GuidedField {
    name: String,
    kind: String,
    description: String,
    required: bool
});
wire_struct!(ToolDefinition {
    name: String,
    description: String,
    schema_json: String
});
wire_struct!(ToolCall {
    id: String,
    name: String,
    arguments_json: String
});
wire_struct!(ToolResult {
    id: String,
    content: String,
    content_json: String
});
wire_struct!(VisionDetection {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64
});
wire_struct!(VisionMask {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    mask_base64: String,
    mask_width: i64,
    mask_height: i64
});
wire_struct!(VisionDepthMap { width: i64, height: i64, values: Vec<f64>, unit: String, minimum: f64, maximum: f64 });

wire_reply!(GetUseCaseAvailability_Reply {
    availability: ModelAvailability
});
wire_reply!(CreateSession_Reply {
    session_id: String,
    profile_id: String
});
wire_reply!(StreamResponse_Reply { token: String });
wire_reply!(StreamRespondGuided_Reply { snapshot_json: String, tool_calls: Vec<ToolCall> });
wire_reply!(StreamSubmitToolResultsGuided_Reply { snapshot_json: String, tool_calls: Vec<ToolCall> });
wire_reply!(StreamEmbed_Reply { embedding: Vec<f64>, embedding_pipeline_id: String });
wire_reply!(StreamTranscribe_Reply { token: String });
wire_reply!(StreamSynthesize_Reply { chunk: AudioChunk });
wire_reply!(StreamDescribe_Reply { token: String });
wire_reply!(StreamOcr_Reply { token: String });
wire_reply!(StreamDetect_Reply { detections: Vec<VisionDetection> });
wire_reply!(StreamSegment_Reply { masks: Vec<VisionMask> });
wire_reply!(StreamDepth_Reply {
    depth: VisionDepthMap
});

pub const CUSTOM_TYPES: &[&zlink::idl::CustomType<'static>] = &[
    ModelAvailability::CUSTOM_TYPE,
    ResponseOptions::CUSTOM_TYPE,
    GuidedOptions::CUSTOM_TYPE,
    EmbedOptions::CUSTOM_TYPE,
    SpeechOptions::CUSTOM_TYPE,
    SynthesisOptions::CUSTOM_TYPE,
    AudioChunk::CUSTOM_TYPE,
    VisionOptions::CUSTOM_TYPE,
    VisionPointPrompt::CUSTOM_TYPE,
    VisionBoxPrompt::CUSTOM_TYPE,
    VisionSegmentOptions::CUSTOM_TYPE,
    GuidedField::CUSTOM_TYPE,
    ToolDefinition::CUSTOM_TYPE,
    ToolCall::CUSTOM_TYPE,
    ToolResult::CUSTOM_TYPE,
    VisionDetection::CUSTOM_TYPE,
    VisionMask::CUSTOM_TYPE,
    VisionDepthMap::CUSTOM_TYPE,
];

#[derive(Clone, Debug, PartialEq, zlink::ReplyError, zlink::introspect::ReplyError)]
#[zlink(interface = "aileron.Inference")]
pub enum Error {
    PermissionPromptRequired { app_id: String, use_case: String },
    PermissionDenied { app_id: String, use_case: String },
    SessionNotFound { session_id: String },
    ModelUnavailable { reason: String },
    InvalidGenerationOptions { reason: String },
    GuidedGenerationFailed { reason: String },
    GenerationFailed { reason: String },
    ContextWindowExceeded { reason: String },
    UnsupportedLanguage { reason: String },
    SafetyRefusal { reason: String },
    RequestCancelled { reason: String },
    InvalidInput { reason: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[zlink::proxy("aileron.Inference")]
pub trait VarlinkClientInterface {
    async fn get_use_case_availability(
        &mut self,
        app_id: String,
        use_case: String,
    ) -> zlink::Result<std::result::Result<GetUseCaseAvailability_Reply, Error>>;
    async fn create_session(
        &mut self,
        app_id: String,
        use_case: String,
        instructions: String,
    ) -> zlink::Result<std::result::Result<CreateSession_Reply, Error>>;
    async fn prewarm(
        &mut self,
        session_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;

    async fn cancel_active_request(
        &mut self,
        session_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;

    async fn end_session(
        &mut self,
        session_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
}

type TokioConnection = zlink::tokio::unix::Connection;

/// Request-owned streaming cursor that avoids zlink 0.7's non-`Send`
/// `ReplyStream` wrapper while retaining native async framing.
#[derive(Debug)]
pub struct InferenceReplyStream<'a, R> {
    connection: &'a mut TokioConnection,
    finished: bool,
    reply: std::marker::PhantomData<R>,
}

impl<R> InferenceReplyStream<'_, R>
where
    R: serde::de::DeserializeOwned + std::fmt::Debug,
{
    pub async fn next(&mut self) -> Option<zlink::Result<std::result::Result<R, Error>>> {
        if self.finished {
            return None;
        }

        let received = self.connection.receive_reply::<R, Error>().await;
        Some(match received {
            Ok((Ok(reply), _fds)) => {
                self.finished = reply.continues() != Some(true);
                reply
                    .into_parameters()
                    .ok_or(zlink::Error::MissingParameters)
                    .map(Ok)
            }
            Ok((Err(error), _fds)) => {
                self.finished = true;
                Ok(Err(error))
            }
            Err(error) => {
                self.finished = true;
                Err(error)
            }
        })
    }
}

#[derive(Debug, Serialize)]
struct MethodCall<P> {
    method: &'static str,
    parameters: P,
}

async fn start_stream<'a, R, P>(
    connection: &'a mut TokioConnection,
    method: &'static str,
    parameters: P,
) -> zlink::Result<InferenceReplyStream<'a, R>>
where
    P: Serialize + std::fmt::Debug,
{
    let call = zlink::Call::new(MethodCall { method, parameters }).set_more(true);
    connection.send_call(&call, Vec::new()).await?;
    Ok(InferenceReplyStream {
        connection,
        finished: false,
        reply: std::marker::PhantomData,
    })
}

macro_rules! stream_params {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Debug, Serialize)]
        struct $name { $(pub $field: $ty),* }
    };
}

stream_params!(StreamResponse_Params { session_id: String, input_json: String, media_paths: Vec<String>, options: ResponseOptions });
stream_params!(StreamRespondGuided_Params { session_id: String, prompt: String, media_paths: Vec<String>, fields: Vec<GuidedField>, tools: Vec<ToolDefinition>, options: GuidedOptions });
stream_params!(StreamSubmitToolResultsGuided_Params { session_id: String, prompt: String, media_paths: Vec<String>, results: Vec<ToolResult>, fields: Vec<GuidedField>, tools: Vec<ToolDefinition>, options: GuidedOptions });
stream_params!(StreamEmbed_Params {
    session_id: String,
    text: String,
    options: EmbedOptions
});
stream_params!(StreamTranscribe_Params {
    session_id: String,
    audio_path: String,
    options: SpeechOptions
});
stream_params!(StreamSynthesize_Params {
    session_id: String,
    text: String,
    options: SynthesisOptions
});
stream_params!(StreamVision_Params {
    session_id: String,
    image_path: String,
    instructions: String,
    options: VisionOptions
});
stream_params!(StreamSegment_Params {
    session_id: String,
    image_path: String,
    instructions: String,
    options: VisionSegmentOptions
});

#[allow(async_fn_in_trait)]
pub trait VarlinkStreamingClientInterface {
    async fn stream_response(
        &mut self,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamResponse_Reply>>;
    async fn stream_respond_guided(
        &mut self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamRespondGuided_Reply>>;
    async fn stream_submit_tool_results_guided(
        &mut self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSubmitToolResultsGuided_Reply>>;
    async fn stream_embed(
        &mut self,
        session_id: String,
        text: String,
        options: EmbedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamEmbed_Reply>>;
    async fn stream_transcribe(
        &mut self,
        session_id: String,
        audio_path: String,
        options: SpeechOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamTranscribe_Reply>>;
    async fn stream_synthesize(
        &mut self,
        session_id: String,
        text: String,
        options: SynthesisOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSynthesize_Reply>>;
    async fn stream_describe(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDescribe_Reply>>;
    async fn stream_ocr(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamOcr_Reply>>;
    async fn stream_detect(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDetect_Reply>>;
    async fn stream_segment(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionSegmentOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSegment_Reply>>;
    async fn stream_depth(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDepth_Reply>>;
}

impl VarlinkStreamingClientInterface for TokioConnection {
    async fn stream_response(
        &mut self,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamResponse_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamResponse",
            StreamResponse_Params {
                session_id,
                input_json,
                media_paths,
                options,
            },
        )
        .await
    }
    async fn stream_respond_guided(
        &mut self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamRespondGuided_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamRespondGuided",
            StreamRespondGuided_Params {
                session_id,
                prompt,
                media_paths,
                fields,
                tools,
                options,
            },
        )
        .await
    }
    async fn stream_submit_tool_results_guided(
        &mut self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSubmitToolResultsGuided_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamSubmitToolResultsGuided",
            StreamSubmitToolResultsGuided_Params {
                session_id,
                prompt,
                media_paths,
                results,
                fields,
                tools,
                options,
            },
        )
        .await
    }
    async fn stream_embed(
        &mut self,
        session_id: String,
        text: String,
        options: EmbedOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamEmbed_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamEmbed",
            StreamEmbed_Params {
                session_id,
                text,
                options,
            },
        )
        .await
    }
    async fn stream_transcribe(
        &mut self,
        session_id: String,
        audio_path: String,
        options: SpeechOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamTranscribe_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamTranscribe",
            StreamTranscribe_Params {
                session_id,
                audio_path,
                options,
            },
        )
        .await
    }
    async fn stream_synthesize(
        &mut self,
        session_id: String,
        text: String,
        options: SynthesisOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSynthesize_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamSynthesize",
            StreamSynthesize_Params {
                session_id,
                text,
                options,
            },
        )
        .await
    }
    async fn stream_describe(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDescribe_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamDescribe",
            StreamVision_Params {
                session_id,
                image_path,
                instructions,
                options,
            },
        )
        .await
    }
    async fn stream_ocr(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamOcr_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamOcr",
            StreamVision_Params {
                session_id,
                image_path,
                instructions,
                options,
            },
        )
        .await
    }
    async fn stream_detect(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDetect_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamDetect",
            StreamVision_Params {
                session_id,
                image_path,
                instructions,
                options,
            },
        )
        .await
    }
    async fn stream_segment(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionSegmentOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamSegment_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamSegment",
            StreamSegment_Params {
                session_id,
                image_path,
                instructions,
                options,
            },
        )
        .await
    }
    async fn stream_depth(
        &mut self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<'_, StreamDepth_Reply>> {
        start_stream(
            self,
            "aileron.Inference.StreamDepth",
            StreamVision_Params {
                session_id,
                image_path,
                instructions,
                options,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_call_uses_more_and_the_declared_wire_shape() {
        let call = zlink::Call::new(MethodCall {
            method: "aileron.Inference.StreamEmbed",
            parameters: StreamEmbed_Params {
                session_id: "session-1".into(),
                text: "hello".into(),
                options: EmbedOptions {
                    execution_mode: "interactive".into(),
                },
            },
        })
        .set_more(true);

        assert_eq!(
            serde_json::to_value(call).unwrap(),
            serde_json::json!({
                "method": "aileron.Inference.StreamEmbed",
                "parameters": {
                    "session_id": "session-1",
                    "text": "hello",
                    "options": { "execution_mode": "interactive" }
                },
                "more": true
            })
        );
    }

    #[test]
    fn portal_streaming_cursor_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<InferenceReplyStream<'static, StreamResponse_Reply>>();
    }

    #[test]
    fn nested_owned_stream_reply_round_trips() {
        let reply = StreamRespondGuided_Reply {
            snapshot_json: "{\"answer\":42}".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "lookup".into(),
                arguments_json: "{}".into(),
            }],
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(
            serde_json::from_str::<StreamRespondGuided_Reply>(&json).unwrap(),
            reply
        );
    }

    #[test]
    fn declared_error_has_stable_name_and_fields() {
        let error = Error::PermissionDenied {
            app_id: "org.example.App".into(),
            use_case: "language.generate".into(),
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "error": "aileron.Inference.PermissionDenied",
                "parameters": { "app_id": "org.example.App", "use_case": "language.generate" }
            })
        );
    }

    #[test]
    fn every_declared_error_round_trips_with_its_qualified_name() {
        let errors = [
            Error::PermissionPromptRequired {
                app_id: "app".into(),
                use_case: "case".into(),
            },
            Error::PermissionDenied {
                app_id: "app".into(),
                use_case: "case".into(),
            },
            Error::SessionNotFound {
                session_id: "session".into(),
            },
            Error::ModelUnavailable {
                reason: "reason".into(),
            },
            Error::InvalidGenerationOptions {
                reason: "reason".into(),
            },
            Error::GuidedGenerationFailed {
                reason: "reason".into(),
            },
            Error::GenerationFailed {
                reason: "reason".into(),
            },
            Error::ContextWindowExceeded {
                reason: "reason".into(),
            },
            Error::UnsupportedLanguage {
                reason: "reason".into(),
            },
            Error::SafetyRefusal {
                reason: "reason".into(),
            },
            Error::RequestCancelled {
                reason: "reason".into(),
            },
            Error::InvalidInput {
                reason: "reason".into(),
            },
        ];
        let names = [
            "PermissionPromptRequired",
            "PermissionDenied",
            "SessionNotFound",
            "ModelUnavailable",
            "InvalidGenerationOptions",
            "GuidedGenerationFailed",
            "GenerationFailed",
            "ContextWindowExceeded",
            "UnsupportedLanguage",
            "SafetyRefusal",
            "RequestCancelled",
            "InvalidInput",
        ];

        for (error, name) in errors.into_iter().zip(names) {
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["error"], format!("aileron.Inference.{name}"));
            assert!(value["parameters"].is_object());
            assert_eq!(serde_json::from_value::<Error>(value).unwrap(), error);
        }
    }
}
