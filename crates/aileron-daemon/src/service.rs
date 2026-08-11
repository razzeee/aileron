//! Native async Varlink service for the daemon.

use tracing::info;

use crate::handlers::{InferenceHandler, ModelsHandler, PermissionsHandler, SessionsHandler};
use crate::state::SharedState;
use aileron_varlink::{inference, models, permissions, sessions};

pub struct AileronService {
    inference: InferenceHandler,
    models: ModelsHandler,
    permissions: PermissionsHandler,
    sessions: SessionsHandler,
}

impl AileronService {
    pub fn new(state: SharedState) -> Self {
        Self {
            inference: InferenceHandler::new(state.clone()),
            models: ModelsHandler::new(state.clone()),
            permissions: PermissionsHandler::new(state.clone()),
            sessions: SessionsHandler::new(state),
        }
    }
}

#[zlink::service(
    vendor = "aileron",
    product = "Aileron local AI daemon",
    version = env!("CARGO_PKG_VERSION"),
    url = "https://github.com/aileron-project/aileron"
)]
impl AileronService {
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
        self.inference
            .get_use_case_availability(app_id, use_case)
            .await
    }

    async fn create_session(
        &self,
        app_id: String,
        use_case: String,
        instructions: String,
    ) -> Result<inference::CreateSession_Reply, inference::Error> {
        self.inference
            .create_session(app_id, use_case, instructions)
            .await
    }

    async fn prewarm(&self, session_id: String) -> Result<(), inference::Error> {
        self.inference.prewarm(session_id).await
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
        self.inference
            .stream_response(more, session_id, input_json, media_paths, options)
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
        self.inference.stream_respond_guided(
            more,
            session_id,
            prompt,
            media_paths,
            fields,
            tools,
            options,
        )
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
        self.inference.stream_submit_tool_results_guided(
            more,
            session_id,
            prompt,
            media_paths,
            results,
            fields,
            tools,
            options,
        )
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
        self.inference.stream_embed(more, session_id, text, options)
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
        self.inference
            .stream_transcribe(more, session_id, audio_path, options)
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
        self.inference
            .stream_synthesize(more, session_id, text, options)
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
        self.inference
            .stream_describe(more, session_id, image_path, instructions, options)
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
        self.inference
            .stream_ocr(more, session_id, image_path, instructions, options)
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
        self.inference
            .stream_detect(more, session_id, image_path, instructions, options)
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
        self.inference
            .stream_segment(more, session_id, image_path, instructions, options)
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
        self.inference
            .stream_depth(more, session_id, image_path, instructions, options)
    }

    async fn cancel_active_request(&self, session_id: String) -> Result<(), inference::Error> {
        self.inference.cancel_active_request(session_id).await
    }

    async fn end_session(&self, session_id: String) -> Result<(), inference::Error> {
        self.inference.end_session(session_id).await
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
        self.models.list().await
    }

    async fn list_runtime_manifests(&self) -> models::ListRuntimeManifests_Reply {
        self.models.list_runtime_manifests().await
    }

    async fn list_runtime_images(&self) -> Result<models::ListRuntimeImages_Reply, models::Error> {
        self.models
            .list_runtime_images()
            .await
            .map_err(|error| models::Error::InstallFailed {
                profile_id: "runtime-images".to_string(),
                reason: error.to_string(),
            })
    }

    async fn remove_runtime_image(&self, image_id: String) -> Result<(), models::Error> {
        self.models
            .remove_runtime_image(image_id.clone())
            .await
            .map_err(|error| models::Error::InstallFailed {
                profile_id: image_id,
                reason: error.to_string(),
            })
    }

    async fn update_runtime_image(&self, image_ref: String) -> Result<(), models::Error> {
        self.models
            .update_runtime_image(image_ref.clone())
            .await
            .map_err(|error| models::Error::InstallFailed {
                profile_id: image_ref,
                reason: error.to_string(),
            })
    }

    async fn prune_unused_runtime_images(
        &self,
    ) -> Result<models::PruneUnusedRuntimeImages_Reply, models::Error> {
        self.models
            .prune_unused_runtime_images()
            .await
            .map_err(|error| models::Error::InstallFailed {
                profile_id: "runtime-images".to_string(),
                reason: error.to_string(),
            })
    }

    async fn list_catalog(&self) -> models::ListCatalog_Reply {
        self.models.list_catalog().await
    }

    async fn list_installs(&self) -> models::ListInstalls_Reply {
        self.models.list_installs().await
    }

    async fn cancel_install(&self, profile_id: String) {
        self.models.cancel_install(profile_id).await;
    }

    #[zlink(more)]
    async fn install_manifest(
        &self,
        _more: bool,
        profile_id: String,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<models::InstallManifest_Reply>, models::Error>,
    > + Unpin {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let models = self.models.clone();
        let cancel_models = models.clone();
        let cancel_profile_id = profile_id.clone();
        let cancellation_sender = sender.clone();
        tokio::spawn(async move {
            let cancellation_task = tokio::spawn(async move {
                cancellation_sender.closed().await;
                loop {
                    cancel_models
                        .cancel_install(cancel_profile_id.clone())
                        .await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
            });
            let item = models
                .install_manifest(profile_id)
                .await
                .map(|reply| zlink::Reply::from(reply).set_continues(Some(false)));
            let _ = sender.send(item).await;
            cancellation_task.abort();
        });
        tokio_stream::wrappers::ReceiverStream::new(receiver)
    }

    #[zlink(more)]
    #[allow(clippy::too_many_arguments)]
    async fn install_url_profile(
        &self,
        _more: bool,
        runtime_id: String,
        url: String,
        sha256: String,
        mmproj_url: String,
        mmproj_sha256: String,
        use_cases: Vec<String>,
    ) -> impl zlink::futures_util::Stream<
        Item = Result<zlink::Reply<models::InstallUrlProfile_Reply>, models::Error>,
    > + Unpin {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let models = self.models.clone();
        let cancel_models = models.clone();
        let cancel_profile_id = ModelsHandler::url_profile_id(&runtime_id, &url, &sha256).ok();
        let cancellation_sender = sender.clone();
        tokio::spawn(async move {
            let cancellation_task = tokio::spawn(async move {
                cancellation_sender.closed().await;
                if let Some(profile_id) = cancel_profile_id {
                    loop {
                        cancel_models.cancel_install(profile_id.clone()).await;
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                }
            });
            let item = models
                .install_url_profile(
                    runtime_id,
                    url,
                    sha256,
                    mmproj_url,
                    mmproj_sha256,
                    use_cases,
                )
                .await
                .map(|reply| zlink::Reply::from(reply).set_continues(Some(false)));
            let _ = sender.send(item).await;
            cancellation_task.abort();
        });
        tokio_stream::wrappers::ReceiverStream::new(receiver)
    }

    async fn delete_profile(&self, profile_id: String, force: bool) -> Result<(), models::Error> {
        self.models.delete_profile(profile_id, force).await
    }

    async fn assign_use_case(
        &self,
        profile_id: String,
        use_case: String,
    ) -> Result<(), models::Error> {
        self.models.assign_use_case(profile_id, use_case).await
    }

    #[zlink(interface = "aileron.Permissions", types = [permissions::AppPermission])]
    async fn list_app_permissions(&self) -> permissions::ListAppPermissions_Reply {
        self.permissions.list_app_permissions().await
    }

    async fn set_app_permission(&self, app_id: String, use_case: String, allowed: bool) {
        if let Err(error) = self
            .permissions
            .set_app_permission(app_id, use_case, allowed)
            .await
        {
            tracing::error!(%error, "failed to persist app permission");
        }
    }

    #[zlink(interface = "aileron.Sessions", types = [sessions::SessionInfo])]
    async fn list_active(&self) -> sessions::ListActive_Reply {
        self.sessions.list_active().await
    }

    async fn kill_session(&self, session_id: String) -> Result<(), sessions::Error> {
        self.sessions.kill_session(session_id).await
    }
}

pub async fn run(state: SharedState) -> anyhow::Result<()> {
    let path = aileron_ipc::server::socket_path();
    info!(path = %path.display(), "listening for Varlink connections");

    let eviction_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            eviction_state.2.lock().await.evict_idle();
        }
    });

    let listener = zlink::tokio::unix::bind(&path)?;
    zlink::Server::new(listener, AileronService::new(state))
        .run()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assignments::Assignments;
    use crate::config::Config;
    use crate::container::ContainerPool;
    use crate::hardware::Variant;
    use crate::manifests::RuntimeManifestStore;
    use crate::permissions::PermissionStore;
    use crate::profiles::ProfileStore;
    use crate::state::Inner;
    use aileron_varlink::inference::VarlinkClientInterface as _;
    use aileron_varlink::inference::VarlinkStreamingClientInterface as _;
    use aileron_varlink::models::VarlinkClientInterface as _;
    use aileron_varlink::permissions::VarlinkClientInterface as _;
    use aileron_varlink::sessions::VarlinkClientInterface as _;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;
    use zlink::futures_util::{StreamExt, pin_mut};
    use zlink::varlink_service::Proxy as _;

    fn test_state(root: &std::path::Path) -> SharedState {
        let config = Config {
            allow_all: true,
            auto_grant: false,
            idle_timeout_secs: 300,
            container_memory: "1g".to_string(),
            oci_store: Some(root.join("oci")),
        };
        let mut containers = ContainerPool::new();
        containers.oci_store = root.join("oci");
        SharedState(
            Arc::new(Mutex::new(Inner {
                config,
                permissions: PermissionStore::default(),
                assignments: Assignments::default(),
                profiles: ProfileStore::default(),
                profile_epochs: HashMap::new(),
                runtimes: RuntimeManifestStore::default(),
                sessions: HashMap::new(),
                installing_profiles: HashMap::new(),
                runtime_downloads: HashMap::new(),
                runtime_download_owners: HashMap::new(),
                runtime_update_checks: HashMap::new(),
                recent_installs: VecDeque::new(),
                recent_runtime_downloads: VecDeque::new(),
                variant: Variant::Cpu,
            })),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(Mutex::new(containers)),
            Arc::new(StdMutex::new(HashSet::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(HashMap::new())),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wire_introspection_calls_errors_streams_and_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("service.socket");
        let listener = zlink::tokio::unix::bind(&socket).unwrap();
        let server = zlink::Server::new(listener, AileronService::new(test_state(dir.path())));
        let client_test = async {
            let mut info_conn = zlink::tokio::unix::connect(&socket).await.unwrap();
            let info = info_conn.get_info().await.unwrap().unwrap();
            assert_eq!(info.vendor, "aileron");
            assert_eq!(info.product, "Aileron local AI daemon");
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
            for (name, expected) in aileron_varlink::INTERFACES {
                let actual = info_conn
                    .get_interface_description(name)
                    .await
                    .unwrap()
                    .unwrap();
                assert_interface_shape(&actual, expected);
            }

            let availability = async {
                let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
                let mut client = connection;
                client
                    .get_use_case_availability("app".into(), "not.supported".into())
                    .await
                    .unwrap()
                    .unwrap()
            };
            let list_models = async {
                let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
                let mut client = connection;
                client.list().await.unwrap().unwrap()
            };
            let list_permissions = async {
                let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
                let mut client = connection;
                client.list_app_permissions().await.unwrap().unwrap()
            };
            let (availability, model_list, permission_list) =
                tokio::join!(availability, list_models, list_permissions);
            assert_eq!(availability.availability.code, "unsupported_use_case");
            assert!(model_list.profiles.is_empty());
            assert!(permission_list.permissions.is_empty());

            let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
            let mut sessions_client = connection;
            assert_eq!(
                sessions_client
                    .kill_session("missing".into())
                    .await
                    .unwrap()
                    .unwrap_err(),
                sessions::Error::SessionNotFound {
                    session_id: "missing".into()
                }
            );

            let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
            let mut inference_client = connection;
            let stream = inference_client
                .stream_embed(
                    "missing".into(),
                    "text".into(),
                    inference::EmbedOptions {
                        execution_mode: "interactive".into(),
                    },
                )
                .await
                .unwrap();
            pin_mut!(stream);
            assert_eq!(
                stream.next().await.unwrap().unwrap().unwrap_err(),
                inference::Error::SessionNotFound {
                    session_id: "missing".into()
                }
            );
            assert!(stream.next().await.is_none());

            let connection = zlink::tokio::unix::connect(&socket).await.unwrap();
            let mut models_client = connection;
            let install_stream = models_client
                .install_manifest("missing-profile".into())
                .await
                .unwrap();
            pin_mut!(install_stream);
            assert!(matches!(
                install_stream.next().await.unwrap().unwrap(),
                Err(models::Error::InstallFailed { profile_id, .. })
                    if profile_id == "missing-profile"
            ));
            assert!(install_stream.next().await.is_none());

            drop(info_conn);
        };
        tokio::select! {
            result = server.run() => panic!("server exited early: {result:?}"),
            () = client_test => {}
        }
        std::fs::remove_file(&socket).unwrap();
        assert!(!socket.exists());
    }

    fn assert_interface_shape(
        actual: &zlink::varlink_service::InterfaceDescription<'_>,
        expected: &str,
    ) {
        let actual = actual.parse().unwrap();
        let expected = zlink::idl::Interface::try_from(expected).unwrap();
        assert_eq!(actual.name(), expected.name());
        assert_eq!(
            actual
                .methods()
                .map(|method| method.name())
                .collect::<Vec<_>>(),
            expected
                .methods()
                .map(|method| method.name())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            actual
                .custom_types()
                .map(|ty| ty.name())
                .collect::<Vec<_>>(),
            expected
                .custom_types()
                .map(|ty| ty.name())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            actual
                .errors()
                .map(|error| error.name())
                .collect::<Vec<_>>(),
            expected
                .errors()
                .map(|error| error.name())
                .collect::<Vec<_>>()
        );
        for expected_method in expected.methods() {
            let actual_method = actual
                .methods()
                .find(|method| method.name() == expected_method.name())
                .unwrap();
            assert_eq!(
                actual_method
                    .inputs()
                    .map(|parameter| (parameter.name(), parameter.ty()))
                    .collect::<Vec<_>>(),
                expected_method
                    .inputs()
                    .map(|parameter| (parameter.name(), parameter.ty()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                actual_method
                    .outputs()
                    .map(|parameter| (parameter.name(), parameter.ty()))
                    .collect::<Vec<_>>(),
                expected_method
                    .outputs()
                    .map(|parameter| (parameter.name(), parameter.ty()))
                    .collect::<Vec<_>>()
            );
        }
        for expected_type in expected.custom_types() {
            let actual_type = actual
                .custom_types()
                .find(|ty| ty.name() == expected_type.name())
                .unwrap();
            let actual_fields = actual_type.as_object().unwrap().fields();
            let expected_fields = expected_type.as_object().unwrap().fields();
            assert_eq!(
                actual_fields
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>(),
                expected_fields
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>()
            );
        }
        for expected_error in expected.errors() {
            let actual_error = actual
                .errors()
                .find(|error| error.name() == expected_error.name())
                .unwrap();
            assert_eq!(
                actual_error
                    .fields()
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>(),
                expected_error
                    .fields()
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>()
            );
        }
    }
}
