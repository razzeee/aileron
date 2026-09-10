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
pub struct InferenceReplyStream<R> {
    connection: Option<TokioConnection>,
    finished: bool,
    reply: std::marker::PhantomData<R>,
}

impl<R> InferenceReplyStream<R>
where
    R: serde::de::DeserializeOwned + std::fmt::Debug,
{
    pub async fn next(&mut self) -> Option<zlink::Result<std::result::Result<R, Error>>> {
        if self.finished {
            return None;
        }

        let received = self
            .connection
            .as_mut()
            .expect("active stream has a connection")
            .receive_reply::<R, Error>()
            .await;
        Some(match received {
            Ok((Ok(reply), _fds)) => {
                self.finished = reply.continues() != Some(true);
                match reply.into_parameters() {
                    Some(parameters) => Ok(Ok(parameters)),
                    None => {
                        self.finished = true;
                        self.connection.take();
                        Err(zlink::Error::MissingParameters)
                    }
                }
            }
            Ok((Err(error), _fds)) => {
                self.finished = true;
                Ok(Err(error))
            }
            Err(error) => {
                self.finished = true;
                self.connection.take();
                Err(error)
            }
        })
    }
}

impl<R> InferenceReplyStream<R> {
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Recover a connection only after receiving a terminal success or typed
    /// error. Abandoning a stream or failing to decode a reply closes it instead.
    pub fn into_connection(self) -> Option<TokioConnection> {
        if self.finished { self.connection } else { None }
    }
}

#[derive(Debug, Serialize)]
struct MethodCall<P> {
    method: &'static str,
    parameters: P,
}

async fn start_stream<R, P>(
    mut connection: TokioConnection,
    method: &'static str,
    parameters: P,
) -> zlink::Result<InferenceReplyStream<R>>
where
    P: Serialize + std::fmt::Debug,
{
    let call = zlink::Call::new(MethodCall { method, parameters }).set_more(true);
    connection.send_call(&call, Vec::new()).await?;
    Ok(InferenceReplyStream {
        connection: Some(connection),
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
pub trait VarlinkStreamingClientInterface: Sized {
    async fn stream_response(
        self,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamResponse_Reply>>;
    async fn stream_respond_guided(
        self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamRespondGuided_Reply>>;
    async fn stream_submit_tool_results_guided(
        self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSubmitToolResultsGuided_Reply>>;
    async fn stream_embed(
        self,
        session_id: String,
        text: String,
        options: EmbedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamEmbed_Reply>>;
    async fn stream_transcribe(
        self,
        session_id: String,
        audio_path: String,
        options: SpeechOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamTranscribe_Reply>>;
    async fn stream_synthesize(
        self,
        session_id: String,
        text: String,
        options: SynthesisOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSynthesize_Reply>>;
    async fn stream_describe(
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDescribe_Reply>>;
    async fn stream_ocr(
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamOcr_Reply>>;
    async fn stream_detect(
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDetect_Reply>>;
    async fn stream_segment(
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionSegmentOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSegment_Reply>>;
    async fn stream_depth(
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDepth_Reply>>;
}

impl VarlinkStreamingClientInterface for TokioConnection {
    async fn stream_response(
        self,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamResponse_Reply>> {
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
        self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamRespondGuided_Reply>> {
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
        self,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        results: Vec<ToolResult>,
        fields: Vec<GuidedField>,
        tools: Vec<ToolDefinition>,
        options: GuidedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSubmitToolResultsGuided_Reply>> {
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
        self,
        session_id: String,
        text: String,
        options: EmbedOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamEmbed_Reply>> {
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
        self,
        session_id: String,
        audio_path: String,
        options: SpeechOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamTranscribe_Reply>> {
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
        self,
        session_id: String,
        text: String,
        options: SynthesisOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSynthesize_Reply>> {
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
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDescribe_Reply>> {
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
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamOcr_Reply>> {
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
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDetect_Reply>> {
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
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionSegmentOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamSegment_Reply>> {
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
        self,
        session_id: String,
        image_path: String,
        instructions: String,
        options: VisionOptions,
    ) -> zlink::Result<InferenceReplyStream<StreamDepth_Reply>> {
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

    #[tokio::test]
    async fn abandoned_stream_closes_socket_and_cannot_return_connection() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        for read_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("stream.socket");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let connection = zlink::tokio::unix::connect(&path).await.unwrap();
            let mut stream = connection
                .stream_embed(
                    "session".into(),
                    "text".into(),
                    EmbedOptions {
                        execution_mode: "interactive".into(),
                    },
                )
                .await
                .unwrap();
            let (peer, _) = listener.accept().await.unwrap();
            let mut peer = BufReader::new(peer);
            peer.read_until(0, &mut Vec::new()).await.unwrap();
            if read_first {
                peer.get_mut().write_all(b"{\"parameters\":{\"embedding\":[1.0],\"embedding_pipeline_id\":\"old\"},\"continues\":true}\0{\"parameters\":{\"embedding\":[2.0],\"embedding_pipeline_id\":\"old\"}}\0").await.unwrap();
                assert_eq!(
                    stream.next().await.unwrap().unwrap().unwrap().embedding,
                    [1.0]
                );
            }
            assert!(stream.into_connection().is_none());
            let mut byte = [0];
            match tokio::time::timeout(std::time::Duration::from_secs(1), peer.read(&mut byte))
                .await
                .unwrap()
            {
                Ok(count) => assert_eq!(count, 0),
                Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
            }
        }
    }

    #[tokio::test]
    async fn stream_connection_reuse_requires_a_valid_terminal_reply() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        for (reply, reusable) in [
            (
                "{\"parameters\":{\"embedding\":[1.0],\"embedding_pipeline_id\":\"done\"}}\0",
                true,
            ),
            (
                "{\"error\":\"aileron.Inference.GenerationFailed\",\"parameters\":{\"reason\":\"failed\"}}\0",
                true,
            ),
            ("{\"parameters\":{\"unexpected\":1}}\0", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("stream.socket");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let connection = zlink::tokio::unix::connect(&path).await.unwrap();
            let mut stream = connection
                .stream_embed(
                    "session".into(),
                    "text".into(),
                    EmbedOptions {
                        execution_mode: "interactive".into(),
                    },
                )
                .await
                .unwrap();
            let (peer, _) = listener.accept().await.unwrap();
            let mut peer = BufReader::new(peer);
            peer.read_until(0, &mut Vec::new()).await.unwrap();
            peer.get_mut().write_all(reply.as_bytes()).await.unwrap();
            assert!(stream.next().await.is_some());
            assert!(stream.next().await.is_none());
            assert_eq!(stream.into_connection().is_some(), reusable);
        }
    }

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
        assert_send::<InferenceReplyStream<StreamResponse_Reply>>();
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
