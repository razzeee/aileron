use aileron_varlink::{inference, models, permissions, sessions};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use zlink::futures_util::StreamExt;
use zlink::varlink_service::Proxy as _;

const WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
struct FixtureState {
    before_reply_exited: Arc<Notify>,
    during_stream_exited: Arc<Notify>,
}

struct WireFixture {
    state: FixtureState,
}

#[zlink::service(
    vendor = "Aileron",
    product = "in-process wire fixture",
    version = "1",
    url = "https://github.com/aileron-project/aileron"
)]
impl WireFixture {
    #[zlink(
        interface = "aileron.Inference",
        types = [
            inference::ModelAvailability,
            inference::ResponseOptions,
            inference::GuidedOptions,
            inference::EmbedOptions,
            inference::SpeechOptions,
            inference::SynthesisOptions,
            inference::AudioChunk,
            inference::VisionOptions,
            inference::VisionPointPrompt,
            inference::VisionBoxPrompt,
            inference::VisionSegmentOptions,
            inference::GuidedField,
            inference::ToolDefinition,
            inference::ToolCall,
            inference::ToolResult,
            inference::VisionDetection,
            inference::VisionMask,
            inference::VisionDepthMap
        ]
    )]
    async fn get_use_case_availability(
        &self,
        app_id: String,
        use_case: String,
    ) -> inference::GetUseCaseAvailability_Reply {
        inference::GetUseCaseAvailability_Reply {
            availability: inference::ModelAvailability {
                is_available: true,
                code: app_id,
                reason: use_case,
            },
        }
    }

    async fn create_session(
        &self,
        app_id: String,
        use_case: String,
        instructions: String,
    ) -> Result<inference::CreateSession_Reply, inference::Error> {
        if let Some(name) = app_id.strip_prefix("error:") {
            return Err(inference_error(name));
        }
        Ok(inference::CreateSession_Reply {
            session_id: format!("{app_id}:{use_case}"),
            profile_id: instructions,
        })
    }

    async fn prewarm(&self, session_id: String) -> Result<(), inference::Error> {
        assert_eq!(session_id, "session");
        Ok(())
    }

    #[zlink(more)]
    async fn stream_response(
        &self,
        more: bool,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: inference::ResponseOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamResponse_Reply>, inference::Error>,
    > + Unpin {
        if session_id == "disconnect-before" || session_id == "disconnect-during" {
            return disconnect_stream(
                session_id == "disconnect-before",
                if session_id == "disconnect-before" {
                    self.state.before_reply_exited.clone()
                } else {
                    self.state.during_stream_exited.clone()
                },
            );
        }
        assert!(more);
        assert_eq!(input_json, "input");
        assert_eq!(media_paths, ["media"]);
        assert_eq!(options, response_options());
        reply_stream([
            inference::StreamResponse_Reply {
                token: format!("{session_id}-1"),
            },
            inference::StreamResponse_Reply {
                token: format!("{session_id}-2"),
            },
        ])
    }

    #[zlink(more)]
    #[allow(clippy::too_many_arguments)]
    async fn stream_respond_guided(
        &self,
        more: bool,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        fields: Vec<inference::GuidedField>,
        tools: Vec<inference::ToolDefinition>,
        options: inference::GuidedOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamRespondGuided_Reply>, inference::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, prompt, media_paths), stream_text_inputs());
        assert_eq!(fields, guided_fields());
        assert_eq!(tools, self::tools());
        assert_eq!(options, guided_options());
        reply_stream([guided_reply("guided-1"), guided_reply("guided-2")])
    }

    #[zlink(more)]
    #[allow(clippy::too_many_arguments)]
    async fn stream_submit_tool_results_guided(
        &self,
        more: bool,
        session_id: String,
        prompt: String,
        media_paths: Vec<String>,
        results: Vec<inference::ToolResult>,
        fields: Vec<inference::GuidedField>,
        tools: Vec<inference::ToolDefinition>,
        options: inference::GuidedOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<
            zlink::Reply<inference::StreamSubmitToolResultsGuided_Reply>,
            inference::Error,
        >,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, prompt, media_paths), stream_text_inputs());
        assert_eq!(results, tool_results());
        assert_eq!(fields, guided_fields());
        assert_eq!(tools, self::tools());
        assert_eq!(options, guided_options());
        reply_stream([submit_reply("submit-1"), submit_reply("submit-2")])
    }

    #[zlink(more)]
    async fn stream_embed(
        &self,
        more: bool,
        session_id: String,
        text: String,
        options: inference::EmbedOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamEmbed_Reply>, inference::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, text), ("session".into(), "text".into()));
        assert_eq!(options, embed_options());
        reply_stream([inference::StreamEmbed_Reply {
            embedding: vec![1.0],
            embedding_pipeline_id: "embed-1".into(),
        }])
    }

    #[zlink(more)]
    async fn stream_transcribe(
        &self,
        more: bool,
        session_id: String,
        audio_path: String,
        options: inference::SpeechOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamTranscribe_Reply>, inference::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, audio_path), ("session".into(), "audio".into()));
        assert_eq!(options, speech_options());
        reply_stream([token_reply("speech-1"), token_reply("speech-2")])
    }

    #[zlink(more)]
    async fn stream_synthesize(
        &self,
        more: bool,
        session_id: String,
        text: String,
        options: inference::SynthesisOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamSynthesize_Reply>, inference::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, text), ("session".into(), "hello".into()));
        assert_eq!(options, synthesis_options());
        reply_stream([synthesis_reply("AQACAA=="), synthesis_reply("")])
    }

    #[zlink(more)]
    async fn stream_describe(
        &self,
        more: bool,
        session_id: String,
        image_path: String,
        instructions: String,
        options: inference::VisionOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamDescribe_Reply>, inference::Error>,
    > + Unpin {
        assert_vision_inputs(more, session_id, image_path, instructions, options);
        reply_stream([describe_reply("describe-1"), describe_reply("describe-2")])
    }

    #[zlink(more)]
    async fn stream_ocr(
        &self,
        more: bool,
        session_id: String,
        image_path: String,
        instructions: String,
        options: inference::VisionOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamOcr_Reply>, inference::Error>,
    > + Unpin {
        assert_vision_inputs(more, session_id, image_path, instructions, options);
        reply_stream([ocr_reply("ocr-1"), ocr_reply("ocr-2")])
    }

    #[zlink(more)]
    async fn stream_detect(
        &self,
        more: bool,
        session_id: String,
        image_path: String,
        instructions: String,
        options: inference::VisionOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamDetect_Reply>, inference::Error>,
    > + Unpin {
        assert_vision_inputs(more, session_id, image_path, instructions, options);
        reply_stream([detect_reply("detect-1")])
    }

    #[zlink(more)]
    async fn stream_segment(
        &self,
        more: bool,
        session_id: String,
        image_path: String,
        instructions: String,
        options: inference::VisionSegmentOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamSegment_Reply>, inference::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!((session_id, image_path, instructions), vision_text_inputs());
        assert_eq!(options, segment_options());
        reply_stream([segment_reply("segment-1")])
    }

    #[zlink(more)]
    async fn stream_depth(
        &self,
        more: bool,
        session_id: String,
        image_path: String,
        instructions: String,
        options: inference::VisionOptions,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<inference::StreamDepth_Reply>, inference::Error>,
    > + Unpin {
        assert_vision_inputs(more, session_id, image_path, instructions, options);
        reply_stream([depth_reply(1.0)])
    }

    async fn cancel_active_request(&self, session_id: String) -> Result<(), inference::Error> {
        assert_eq!(session_id, "session");
        Ok(())
    }

    async fn end_session(&self, session_id: String) -> Result<(), inference::Error> {
        assert_eq!(session_id, "session");
        Ok(())
    }

    #[zlink(
        interface = "aileron.Models",
        types = [
            models::RuntimeImage,
            models::ProfileInfo,
            models::RuntimeManifestInfo,
            models::OciRuntimeImage,
            models::RuntimeImageCleanupError,
            models::UseCaseFitScore,
            models::FitScoreComponents,
            models::CatalogProfileInfo,
            models::InstallProgress,
            models::InstallStatus,
            models::UseCaseConflict
        ]
    )]
    async fn list(&self) -> models::List_Reply {
        models::List_Reply {
            profiles: vec![profile()],
        }
    }

    async fn list_runtime_manifests(&self) -> models::ListRuntimeManifests_Reply {
        models::ListRuntimeManifests_Reply {
            runtimes: vec![models::RuntimeManifestInfo {
                runtime_id: "runtime".into(),
                variants: vec!["cpu".into()],
            }],
        }
    }

    async fn list_runtime_images(&self) -> Result<models::ListRuntimeImages_Reply, models::Error> {
        Ok(models::ListRuntimeImages_Reply {
            images: vec![runtime_image()],
        })
    }

    async fn remove_runtime_image(&self, image_id: String) -> Result<(), models::Error> {
        assert_eq!(image_id, "image");
        Ok(())
    }

    async fn update_runtime_image(&self, image_ref: String) -> Result<(), models::Error> {
        assert_eq!(image_ref, "image-ref");
        Ok(())
    }

    async fn prune_unused_runtime_images(
        &self,
    ) -> Result<models::PruneUnusedRuntimeImages_Reply, models::Error> {
        Ok(models::PruneUnusedRuntimeImages_Reply {
            removed: vec!["removed".into()],
            errors: vec![models::RuntimeImageCleanupError {
                image_ref: "kept".into(),
                reason: "busy".into(),
            }],
        })
    }

    async fn list_catalog(&self) -> models::ListCatalog_Reply {
        models::ListCatalog_Reply {
            profiles: vec![catalog_profile()],
        }
    }

    async fn list_installs(&self) -> models::ListInstalls_Reply {
        models::ListInstalls_Reply {
            installs: vec![install_status()],
        }
    }

    async fn cancel_install(&self, profile_id: String) {
        assert_eq!(profile_id, "profile");
    }

    #[zlink(more)]
    async fn install_manifest(
        &self,
        more: bool,
        profile_id: String,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<models::InstallManifest_Reply>, models::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!(profile_id, "profile");
        reply_stream([manifest_reply(2, true)])
    }

    #[zlink(more)]
    #[allow(clippy::too_many_arguments)]
    async fn install_url_profile(
        &self,
        more: bool,
        runtime_id: String,
        url: String,
        sha256: String,
        mmproj_url: String,
        mmproj_sha256: String,
        use_cases: Vec<String>,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<models::InstallUrlProfile_Reply>, models::Error>,
    > + Unpin {
        assert!(more);
        assert_eq!(
            (
                runtime_id,
                url,
                sha256,
                mmproj_url,
                mmproj_sha256,
                use_cases
            ),
            url_install_inputs()
        );
        reply_stream([url_reply(2, true)])
    }

    async fn delete_profile(&self, profile_id: String, force: bool) -> Result<(), models::Error> {
        assert_eq!((profile_id, force), ("profile".into(), true));
        Ok(())
    }

    async fn assign_use_case(
        &self,
        profile_id: String,
        use_case: String,
    ) -> Result<(), models::Error> {
        if let Some(name) = profile_id.strip_prefix("error:") {
            return Err(models_error(name));
        }
        assert_eq!(
            (profile_id, use_case),
            ("profile".into(), "use-case".into())
        );
        Ok(())
    }

    #[zlink(interface = "aileron.Permissions", types = [permissions::AppPermission])]
    async fn list_app_permissions(&self) -> permissions::ListAppPermissions_Reply {
        permissions::ListAppPermissions_Reply {
            permissions: vec![permission()],
        }
    }

    async fn set_app_permission(&self, app_id: String, use_case: String, allowed: bool) {
        assert_eq!(
            (app_id, use_case, allowed),
            ("app".into(), "use-case".into(), true)
        );
    }

    #[zlink(interface = "aileron.Sessions", types = [sessions::SessionInfo])]
    async fn list_active(&self) -> sessions::ListActive_Reply {
        sessions::ListActive_Reply {
            sessions: vec![session()],
        }
    }

    async fn kill_session(&self, session_id: String) -> Result<(), sessions::Error> {
        if session_id == "missing" {
            Err(sessions::Error::SessionNotFound { session_id })
        } else {
            assert_eq!(session_id, "session");
            Ok(())
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn exhaustive_hardware_free_wire_contract() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("wire.socket");
    let listener = zlink::tokio::unix::bind(&socket).unwrap();
    assert!(socket.exists(), "binding creates the service socket");
    let state = FixtureState::default();
    let server = zlink::Server::new(
        listener,
        WireFixture {
            state: state.clone(),
        },
    );
    let clients = exercise_wire_contract(socket.clone(), state);
    let mut server = Box::pin(server.run());
    tokio::select! {
        result = &mut server => panic!("fixture server exited early: {result:?}"),
        () = clients => {}
    }
    drop(server);

    assert!(
        zlink::tokio::unix::connect(&socket).await.is_err(),
        "dropping the server closes its listener"
    );
    std::fs::remove_file(&socket).unwrap();
    assert!(!socket.exists());
}

async fn exercise_wire_contract(socket: std::path::PathBuf, state: FixtureState) {
    introspection_is_exact(&socket).await;
    ordinary_methods_round_trip(&socket).await;
    declared_errors_round_trip(&socket).await;
    streams_round_trip_and_terminate(&socket).await;
    simultaneous_connections_are_independent(&socket).await;
    disconnected_producers_exit(&socket, state).await;
}

async fn introspection_is_exact(socket: &std::path::Path) {
    let mut connection = zlink::tokio::unix::connect(socket).await.unwrap();
    let info = connection.get_info().await.unwrap().unwrap();
    assert_eq!(info.vendor, "Aileron");
    assert_eq!(info.product, "in-process wire fixture");
    assert_eq!(
        info.interfaces,
        [
            "aileron.Inference",
            "aileron.Models",
            "aileron.Permissions",
            "aileron.Sessions",
            "org.varlink.service",
        ]
    );
    for (name, checked_in) in aileron_varlink::INTERFACES {
        let generated = connection
            .get_interface_description(name)
            .await
            .unwrap()
            .unwrap();
        assert_interface_shape(&generated, checked_in);
    }
}

async fn ordinary_methods_round_trip(socket: &std::path::Path) {
    use inference::VarlinkClientInterface as _;
    use models::VarlinkClientInterface as _;
    use permissions::VarlinkClientInterface as _;
    use sessions::VarlinkClientInterface as _;

    let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
    let availability = client
        .get_use_case_availability("app".into(), "use-case".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(availability.availability.code, "app");
    assert_eq!(availability.availability.reason, "use-case");
    assert_eq!(
        client
            .create_session("app".into(), "use-case".into(), "instructions".into())
            .await
            .unwrap()
            .unwrap(),
        inference::CreateSession_Reply {
            session_id: "app:use-case".into(),
            profile_id: "instructions".into(),
        }
    );
    client.prewarm("session".into()).await.unwrap().unwrap();
    client
        .cancel_active_request("session".into())
        .await
        .unwrap()
        .unwrap();
    client.end_session("session".into()).await.unwrap().unwrap();

    assert_eq!(client.list().await.unwrap().unwrap().profiles, [profile()]);
    assert_eq!(
        client
            .list_runtime_manifests()
            .await
            .unwrap()
            .unwrap()
            .runtimes[0]
            .runtime_id,
        "runtime"
    );
    assert_eq!(
        client.list_runtime_images().await.unwrap().unwrap().images,
        [runtime_image()]
    );
    client
        .remove_runtime_image("image".into())
        .await
        .unwrap()
        .unwrap();
    client
        .update_runtime_image("image-ref".into())
        .await
        .unwrap()
        .unwrap();
    let pruned = client.prune_unused_runtime_images().await.unwrap().unwrap();
    assert_eq!(pruned.removed, ["removed"]);
    assert_eq!(pruned.errors[0].reason, "busy");
    assert_eq!(
        client.list_catalog().await.unwrap().unwrap().profiles,
        [catalog_profile()]
    );
    assert_eq!(
        client.list_installs().await.unwrap().unwrap().installs,
        [install_status()]
    );
    client
        .cancel_install("profile".into())
        .await
        .unwrap()
        .unwrap();
    client
        .delete_profile("profile".into(), true)
        .await
        .unwrap()
        .unwrap();
    client
        .assign_use_case("profile".into(), "use-case".into())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        client
            .list_app_permissions()
            .await
            .unwrap()
            .unwrap()
            .permissions,
        [permission()]
    );
    client
        .set_app_permission("app".into(), "use-case".into(), true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        client.list_active().await.unwrap().unwrap().sessions,
        [session()]
    );
    client
        .kill_session("session".into())
        .await
        .unwrap()
        .unwrap();
}

async fn declared_errors_round_trip(socket: &std::path::Path) {
    use inference::VarlinkClientInterface as _;
    use models::VarlinkClientInterface as _;
    use sessions::VarlinkClientInterface as _;

    for name in [
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
    ] {
        let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
        let actual = client
            .create_session(format!("error:{name}"), "ignored".into(), "ignored".into())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(actual, inference_error(name));
    }
    for name in [
        "ProfileNotFound",
        "ProfileInUse",
        "InstallFailed",
        "UnsupportedUseCase",
    ] {
        let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
        let actual = client
            .assign_use_case(format!("error:{name}"), "ignored".into())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(actual, models_error(name));
    }
    let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
    assert_eq!(
        client
            .kill_session("missing".into())
            .await
            .unwrap()
            .unwrap_err(),
        sessions::Error::SessionNotFound {
            session_id: "missing".into()
        }
    );
}

async fn streams_round_trip_and_terminate(socket: &std::path::Path) {
    use inference::VarlinkStreamingClientInterface as _;
    use models::VarlinkClientInterface as _;

    let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
    let mut response = client
        .stream_response(
            "session".into(),
            "input".into(),
            vec!["media".into()],
            response_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut response, |reply| reply.token.clone()).await,
        ["session-1", "session-2"]
    );

    let mut guided = client
        .stream_respond_guided(
            "session".into(),
            "prompt".into(),
            vec!["media".into()],
            guided_fields(),
            tools(),
            guided_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut guided, |reply| reply.snapshot_json.clone()).await,
        ["guided-1", "guided-2"]
    );

    let mut submit = client
        .stream_submit_tool_results_guided(
            "session".into(),
            "prompt".into(),
            vec!["media".into()],
            tool_results(),
            guided_fields(),
            tools(),
            guided_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut submit, |reply| reply.snapshot_json.clone()).await,
        ["submit-1", "submit-2"]
    );

    let mut embed = client
        .stream_embed("session".into(), "text".into(), embed_options())
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut embed, |reply| reply.embedding_pipeline_id.clone()).await,
        ["embed-1"]
    );
    let mut speech = client
        .stream_transcribe("session".into(), "audio".into(), speech_options())
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut speech, |reply| reply.token.clone()).await,
        ["speech-1", "speech-2"]
    );
    let mut synthesis = client
        .stream_synthesize("session".into(), "hello".into(), synthesis_options())
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut synthesis, |reply| reply.chunk.audio_base64.clone()).await,
        ["AQACAA==", ""]
    );
    let mut describe = client
        .stream_describe(
            "session".into(),
            "image".into(),
            "instructions".into(),
            vision_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut describe, |reply| reply.token.clone()).await,
        ["describe-1", "describe-2"]
    );
    let mut ocr = client
        .stream_ocr(
            "session".into(),
            "image".into(),
            "instructions".into(),
            vision_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut ocr, |reply| reply.token.clone()).await,
        ["ocr-1", "ocr-2"]
    );
    let mut detect = client
        .stream_detect(
            "session".into(),
            "image".into(),
            "instructions".into(),
            vision_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut detect, |reply| reply.detections[0].label.clone()).await,
        ["detect-1"]
    );
    let mut segment = client
        .stream_segment(
            "session".into(),
            "image".into(),
            "instructions".into(),
            segment_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut segment, |reply| reply.masks[0].label.clone()).await,
        ["segment-1"]
    );
    let mut depth = client
        .stream_depth(
            "session".into(),
            "image".into(),
            "instructions".into(),
            vision_options(),
        )
        .await
        .unwrap();
    assert_eq!(
        collect_inference(&mut depth, |reply| reply.depth.values[0]).await,
        [1.0]
    );

    let install = client.install_manifest("profile".into()).await.unwrap();
    assert_eq!(
        collect_generated(install, |reply| reply.progress.bytes_pulled).await,
        [2]
    );
    let (runtime_id, url, sha256, mmproj_url, mmproj_sha256, use_cases) = url_install_inputs();
    let install = client
        .install_url_profile(
            runtime_id,
            url,
            sha256,
            mmproj_url,
            mmproj_sha256,
            use_cases,
        )
        .await
        .unwrap();
    assert_eq!(
        collect_generated(install, |reply| reply.progress.bytes_pulled).await,
        [2]
    );
}

async fn collect_inference<R, T>(
    stream: &mut inference::InferenceReplyStream<'_, R>,
    map: impl Fn(&R) -> T,
) -> Vec<T>
where
    R: serde::de::DeserializeOwned + std::fmt::Debug,
{
    let mut values = Vec::new();
    while let Some(reply) = stream.next().await {
        values.push(map(&reply.unwrap().unwrap()));
    }
    values
}

async fn collect_generated<S, R, T>(stream: S, map: impl Fn(&R) -> T) -> Vec<T>
where
    S: zlink::futures_util::Stream<Item = zlink::Result<Result<R, models::Error>>>,
{
    zlink::futures_util::pin_mut!(stream);
    let mut values = Vec::new();
    while let Some(reply) = stream.next().await {
        values.push(map(&reply.unwrap().unwrap()));
    }
    values
}

#[derive(Debug, Serialize)]
struct RawMethodCall {
    method: &'static str,
    parameters: Value,
}

async fn simultaneous_connections_are_independent(socket: &std::path::Path) {
    use inference::VarlinkClientInterface as _;

    let call = |app: &'static str| async move {
        let mut client = zlink::tokio::unix::connect(socket).await.unwrap();
        client
            .get_use_case_availability(app.into(), format!("{app}-case"))
            .await
            .unwrap()
            .unwrap()
    };
    let (one, two) = tokio::join!(call("one"), call("two"));
    assert_eq!(
        (one.availability.code, one.availability.reason),
        ("one".into(), "one-case".into())
    );
    assert_eq!(
        (two.availability.code, two.availability.reason),
        ("two".into(), "two-case".into())
    );
}

async fn disconnected_producers_exit(socket: &std::path::Path, state: FixtureState) {
    send_stream_call_and_disconnect(socket, "disconnect-before", false).await;
    tokio::time::timeout(WAIT, state.before_reply_exited.notified())
        .await
        .expect("producer did not exit within two seconds after pre-reply disconnect");

    send_stream_call_and_disconnect(socket, "disconnect-during", true).await;
    tokio::time::timeout(WAIT, state.during_stream_exited.notified())
        .await
        .expect("producer did not exit within two seconds after stream disconnect");
}

async fn send_stream_call_and_disconnect(
    socket: &std::path::Path,
    session_id: &str,
    receive_first: bool,
) {
    let mut connection = zlink::tokio::unix::connect(socket).await.unwrap();
    let call = zlink::Call::new(RawMethodCall {
        method: "aileron.Inference.StreamResponse",
        parameters: json!({
            "session_id": session_id,
            "input_json": "input",
            "media_paths": ["media"],
            "options": response_options(),
        }),
    })
    .set_more(true);
    connection.send_call(&call, Vec::new()).await.unwrap();
    if receive_first {
        let (reply, _) = connection
            .receive_reply::<inference::StreamResponse_Reply, inference::Error>()
            .await
            .unwrap();
        assert!(reply.is_ok());
    }
}

fn disconnect_stream(
    before_first_reply: bool,
    exited: Arc<Notify>,
) -> ReceiverStream<Result<zlink::Reply<inference::StreamResponse_Reply>, inference::Error>> {
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        if before_first_reply {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut sequence = 0;
        loop {
            sequence += 1;
            let reply = zlink::Reply::new(Some(inference::StreamResponse_Reply {
                token: sequence.to_string(),
            }))
            .set_continues(Some(true));
            if sender.send(Ok(reply)).await.is_err() {
                exited.notify_one();
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    ReceiverStream::new(receiver)
}

fn reply_stream<T, E, const N: usize>(replies: [T; N]) -> ReceiverStream<Result<zlink::Reply<T>, E>>
where
    T: Send + 'static,
    E: Send + 'static,
{
    let (sender, receiver) = mpsc::channel(N.max(1));
    tokio::spawn(async move {
        for (index, reply) in replies.into_iter().enumerate() {
            let continues = index + 1 < N;
            if sender
                .send(Ok(
                    zlink::Reply::new(Some(reply)).set_continues(Some(continues))
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    ReceiverStream::new(receiver)
}

fn inference_error(name: &str) -> inference::Error {
    match name {
        "PermissionPromptRequired" => inference::Error::PermissionPromptRequired {
            app_id: "app".into(),
            use_case: "case".into(),
        },
        "PermissionDenied" => inference::Error::PermissionDenied {
            app_id: "app".into(),
            use_case: "case".into(),
        },
        "SessionNotFound" => inference::Error::SessionNotFound {
            session_id: "session".into(),
        },
        "ModelUnavailable" => inference::Error::ModelUnavailable {
            reason: "reason".into(),
        },
        "InvalidGenerationOptions" => inference::Error::InvalidGenerationOptions {
            reason: "reason".into(),
        },
        "GuidedGenerationFailed" => inference::Error::GuidedGenerationFailed {
            reason: "reason".into(),
        },
        "GenerationFailed" => inference::Error::GenerationFailed {
            reason: "reason".into(),
        },
        "ContextWindowExceeded" => inference::Error::ContextWindowExceeded {
            reason: "reason".into(),
        },
        "UnsupportedLanguage" => inference::Error::UnsupportedLanguage {
            reason: "reason".into(),
        },
        "SafetyRefusal" => inference::Error::SafetyRefusal {
            reason: "reason".into(),
        },
        "RequestCancelled" => inference::Error::RequestCancelled {
            reason: "reason".into(),
        },
        "InvalidInput" => inference::Error::InvalidInput {
            reason: "reason".into(),
        },
        _ => panic!("unknown inference error {name}"),
    }
}

fn models_error(name: &str) -> models::Error {
    match name {
        "ProfileNotFound" => models::Error::ProfileNotFound {
            profile_id: "profile".into(),
        },
        "ProfileInUse" => models::Error::ProfileInUse {
            profile_id: "profile".into(),
        },
        "InstallFailed" => models::Error::InstallFailed {
            profile_id: "profile".into(),
            reason: "reason".into(),
        },
        "UnsupportedUseCase" => models::Error::UnsupportedUseCase {
            profile_id: "profile".into(),
            use_case: "case".into(),
        },
        _ => panic!("unknown models error {name}"),
    }
}

fn response_options() -> inference::ResponseOptions {
    inference::ResponseOptions {
        maximum_response_tokens: 7,
        temperature: 0.5,
        source_language_hint: "de".into(),
        target_language_hint: "en".into(),
        execution_mode: "interactive".into(),
    }
}

fn guided_options() -> inference::GuidedOptions {
    inference::GuidedOptions {
        maximum_response_tokens: 7,
        temperature: 0.5,
        execution_mode: "interactive".into(),
    }
}

fn embed_options() -> inference::EmbedOptions {
    inference::EmbedOptions {
        execution_mode: "interactive".into(),
    }
}

fn speech_options() -> inference::SpeechOptions {
    inference::SpeechOptions {
        source_language_hint: "de".into(),
        execution_mode: "interactive".into(),
    }
}

fn synthesis_options() -> inference::SynthesisOptions {
    inference::SynthesisOptions {
        voice_id: "default".into(),
        language_hint: "en".into(),
        execution_mode: "interactive".into(),
    }
}

fn vision_options() -> inference::VisionOptions {
    inference::VisionOptions {
        execution_mode: "interactive".into(),
    }
}

fn segment_options() -> inference::VisionSegmentOptions {
    inference::VisionSegmentOptions {
        execution_mode: "interactive".into(),
        points: vec![inference::VisionPointPrompt {
            x: 0.1,
            y: 0.2,
            positive: true,
        }],
        boxes: vec![inference::VisionBoxPrompt {
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
        }],
    }
}

fn guided_fields() -> Vec<inference::GuidedField> {
    vec![inference::GuidedField {
        name: "answer".into(),
        kind: "string".into(),
        description: "answer".into(),
        required: true,
    }]
}

fn tools() -> Vec<inference::ToolDefinition> {
    vec![inference::ToolDefinition {
        name: "tool".into(),
        description: "tool".into(),
        schema_json: "{}".into(),
    }]
}

fn tool_calls() -> Vec<inference::ToolCall> {
    vec![inference::ToolCall {
        id: "call".into(),
        name: "tool".into(),
        arguments_json: "{}".into(),
    }]
}

fn tool_results() -> Vec<inference::ToolResult> {
    vec![inference::ToolResult {
        id: "call".into(),
        content: "result".into(),
        content_json: "{}".into(),
    }]
}

fn stream_text_inputs() -> (String, String, Vec<String>) {
    ("session".into(), "prompt".into(), vec!["media".into()])
}

fn vision_text_inputs() -> (String, String, String) {
    ("session".into(), "image".into(), "instructions".into())
}

fn assert_vision_inputs(
    more: bool,
    session_id: String,
    image_path: String,
    instructions: String,
    options: inference::VisionOptions,
) {
    assert!(more);
    assert_eq!((session_id, image_path, instructions), vision_text_inputs());
    assert_eq!(options, vision_options());
}

fn guided_reply(snapshot: &str) -> inference::StreamRespondGuided_Reply {
    inference::StreamRespondGuided_Reply {
        snapshot_json: snapshot.into(),
        tool_calls: tool_calls(),
    }
}

fn submit_reply(snapshot: &str) -> inference::StreamSubmitToolResultsGuided_Reply {
    inference::StreamSubmitToolResultsGuided_Reply {
        snapshot_json: snapshot.into(),
        tool_calls: tool_calls(),
    }
}

fn token_reply(token: &str) -> inference::StreamTranscribe_Reply {
    inference::StreamTranscribe_Reply {
        token: token.into(),
    }
}

fn synthesis_reply(audio_base64: &str) -> inference::StreamSynthesize_Reply {
    inference::StreamSynthesize_Reply {
        chunk: inference::AudioChunk {
            audio_base64: audio_base64.into(),
            sample_rate: 24_000,
            channels: 1,
            sample_format: "s16le".into(),
        },
    }
}

fn describe_reply(token: &str) -> inference::StreamDescribe_Reply {
    inference::StreamDescribe_Reply {
        token: token.into(),
    }
}

fn ocr_reply(token: &str) -> inference::StreamOcr_Reply {
    inference::StreamOcr_Reply {
        token: token.into(),
    }
}

fn detect_reply(label: &str) -> inference::StreamDetect_Reply {
    inference::StreamDetect_Reply {
        detections: vec![inference::VisionDetection {
            label: label.into(),
            confidence: 0.9,
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
        }],
    }
}

fn segment_reply(label: &str) -> inference::StreamSegment_Reply {
    inference::StreamSegment_Reply {
        masks: vec![inference::VisionMask {
            label: label.into(),
            confidence: 0.9,
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
            mask_base64: "AA==".into(),
            mask_width: 1,
            mask_height: 1,
        }],
    }
}

fn depth_reply(value: f64) -> inference::StreamDepth_Reply {
    inference::StreamDepth_Reply {
        depth: inference::VisionDepthMap {
            width: 1,
            height: 1,
            values: vec![value],
            unit: "meter".into(),
            minimum: 0.0,
            maximum: 2.0,
        },
    }
}

fn profile() -> models::ProfileInfo {
    models::ProfileInfo {
        profile_id: "profile".into(),
        model_id: "model".into(),
        runtime_id: "runtime".into(),
        artifact_path: "/model".into(),
        runtime_images: vec![models::RuntimeImage {
            variant: "cpu".into(),
            image_ref: "image-ref".into(),
        }],
        use_cases: vec!["use-case".into()],
        specializations: Some(vec!["special".into()]),
        assigned_use_cases: vec!["use-case".into()],
        size_bytes: 10,
        installed_at: "now".into(),
        source: "fixture".into(),
    }
}

fn runtime_image() -> models::OciRuntimeImage {
    models::OciRuntimeImage {
        image_id: "image".into(),
        image_ref: "image-ref".into(),
        runtime_id: "runtime".into(),
        variant: "cpu".into(),
        size_bytes: 10,
        in_use: true,
        used_by_profiles: vec!["profile".into()],
        update_available: true,
        update_status: "available".into(),
        source: "fixture".into(),
    }
}

fn catalog_profile() -> models::CatalogProfileInfo {
    models::CatalogProfileInfo {
        profile_id: "profile".into(),
        model_id: "model".into(),
        llmfit_model_id: "fit-model".into(),
        llmfit_provider: Some("provider".into()),
        parameter_count: Some("1B".into()),
        quantization: Some("q4".into()),
        context_length: Some(1024),
        release_date: Some("today".into()),
        capabilities: Some(vec!["capability".into()]),
        supported_languages: Some(vec!["en".into()]),
        spdx_license: Some("MIT".into()),
        runtime_id: "runtime".into(),
        tier: "balanced".into(),
        disk_size_gb: 1.0,
        min_ram_gb: 2.0,
        recommended_ram_gb: 3.0,
        min_vram_gb: 4.0,
        fit_score: 0.9,
        use_case_fit_scores: vec![models::UseCaseFitScore {
            use_case: "use-case".into(),
            score: 0.8,
        }],
        fit_level: "good".into(),
        run_mode: Some("cpu".into()),
        inference_runtime: Some("runtime".into()),
        memory_required_gb: Some(2.0),
        memory_available_gb: Some(4.0),
        utilization_pct: Some(50.0),
        estimated_tps: Some(10.0),
        best_quant: Some("q4".into()),
        effective_context_length: Some(512),
        fit_notes: Some(vec!["note".into()]),
        score_components: Some(models::FitScoreComponents {
            quality: 1.0,
            speed: 2.0,
            fit: 3.0,
            context: 4.0,
        }),
        recommended: true,
        installing: false,
        recommendation_reason: "best".into(),
        use_cases: vec!["use-case".into()],
        specializations: Some(vec!["special".into()]),
    }
}

fn install_status() -> models::InstallStatus {
    models::InstallStatus {
        profile_id: "profile".into(),
        bytes_pulled: 1,
        total_bytes: 2,
        bytes_per_second: 3,
        eta_seconds: 4,
        status: "pulling".into(),
        cancel_requested: false,
    }
}

fn install_progress(bytes: i64, done: bool) -> models::InstallProgress {
    models::InstallProgress {
        profile_id: "profile".into(),
        bytes_pulled: bytes,
        total_bytes: 2,
        done,
    }
}

fn conflicts() -> Vec<models::UseCaseConflict> {
    vec![models::UseCaseConflict {
        use_case: "use-case".into(),
        current_profile: "old".into(),
        new_profile: "profile".into(),
    }]
}

fn manifest_reply(bytes: i64, done: bool) -> models::InstallManifest_Reply {
    models::InstallManifest_Reply {
        progress: install_progress(bytes, done),
        auto_assigned: vec!["assigned".into()],
        conflicts: conflicts(),
    }
}

fn url_reply(bytes: i64, done: bool) -> models::InstallUrlProfile_Reply {
    models::InstallUrlProfile_Reply {
        progress: install_progress(bytes, done),
        auto_assigned: vec!["assigned".into()],
        conflicts: conflicts(),
    }
}

fn url_install_inputs() -> (String, String, String, String, String, Vec<String>) {
    (
        "runtime".into(),
        "https://model".into(),
        "sha".into(),
        "https://mmproj".into(),
        "mmsha".into(),
        vec!["use-case".into()],
    )
}

fn permission() -> permissions::AppPermission {
    permissions::AppPermission {
        app_id: "app".into(),
        use_case: "use-case".into(),
        allowed: true,
        last_used: Some("now".into()),
    }
}

fn session() -> sessions::SessionInfo {
    sessions::SessionInfo {
        session_id: "session".into(),
        app_id: "app".into(),
        use_case: "use-case".into(),
        profile_id: "profile".into(),
        started_at: "now".into(),
    }
}

fn assert_interface_shape(
    actual: &zlink::varlink_service::InterfaceDescription<'_>,
    expected: &str,
) {
    let actual = actual.parse().unwrap();
    let expected = zlink::idl::Interface::try_from(expected).unwrap();
    assert_eq!(actual.name(), expected.name());
    assert_eq!(
        actual.methods().map(|item| item.name()).collect::<Vec<_>>(),
        expected
            .methods()
            .map(|item| item.name())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        actual
            .custom_types()
            .map(|item| item.name())
            .collect::<Vec<_>>(),
        expected
            .custom_types()
            .map(|item| item.name())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        actual.errors().map(|item| item.name()).collect::<Vec<_>>(),
        expected
            .errors()
            .map(|item| item.name())
            .collect::<Vec<_>>()
    );
    for expected_method in expected.methods() {
        let actual_method = actual
            .methods()
            .find(|item| item.name() == expected_method.name())
            .unwrap();
        assert_eq!(
            actual_method
                .inputs()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>(),
            expected_method
                .inputs()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            actual_method
                .outputs()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>(),
            expected_method
                .outputs()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>()
        );
    }
    for expected_type in expected.custom_types() {
        let actual_type = actual
            .custom_types()
            .find(|item| item.name() == expected_type.name())
            .unwrap();
        assert_eq!(
            actual_type
                .as_object()
                .unwrap()
                .fields()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>(),
            expected_type
                .as_object()
                .unwrap()
                .fields()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>()
        );
    }
    for expected_error in expected.errors() {
        let actual_error = actual
            .errors()
            .find(|item| item.name() == expected_error.name())
            .unwrap();
        assert_eq!(
            actual_error
                .fields()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>(),
            expected_error
                .fields()
                .map(|item| (item.name(), item.ty()))
                .collect::<Vec<_>>()
        );
    }
}
