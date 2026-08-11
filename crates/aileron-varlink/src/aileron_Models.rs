#![allow(non_camel_case_types)]

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

wire_struct!(RuntimeImage {
    variant: String,
    image_ref: String
});
wire_struct!(ProfileInfo { profile_id: String, model_id: String, runtime_id: String, artifact_path: String, runtime_images: Vec<RuntimeImage>, use_cases: Vec<String>, specializations: Option<Vec<String>>, assigned_use_cases: Vec<String>, size_bytes: i64, installed_at: String, source: String });
wire_struct!(RuntimeManifestInfo { runtime_id: String, variants: Vec<String> });
wire_struct!(OciRuntimeImage { image_id: String, image_ref: String, runtime_id: String, variant: String, size_bytes: i64, in_use: bool, used_by_profiles: Vec<String>, update_available: bool, update_status: String, source: String });
wire_struct!(RuntimeImageCleanupError {
    image_ref: String,
    reason: String
});
wire_struct!(UseCaseFitScore {
    use_case: String,
    score: f64
});
wire_struct!(FitScoreComponents {
    quality: f64,
    speed: f64,
    fit: f64,
    context: f64
});
wire_struct!(CatalogProfileInfo { profile_id: String, model_id: String, llmfit_model_id: String, llmfit_provider: Option<String>, parameter_count: Option<String>, quantization: Option<String>, context_length: Option<i64>, release_date: Option<String>, capabilities: Option<Vec<String>>, supported_languages: Option<Vec<String>>, spdx_license: Option<String>, runtime_id: String, tier: String, disk_size_gb: f64, min_ram_gb: f64, recommended_ram_gb: f64, min_vram_gb: f64, fit_score: f64, use_case_fit_scores: Vec<UseCaseFitScore>, fit_level: String, run_mode: Option<String>, inference_runtime: Option<String>, memory_required_gb: Option<f64>, memory_available_gb: Option<f64>, utilization_pct: Option<f64>, estimated_tps: Option<f64>, best_quant: Option<String>, effective_context_length: Option<i64>, fit_notes: Option<Vec<String>>, score_components: Option<FitScoreComponents>, recommended: bool, installing: bool, recommendation_reason: String, use_cases: Vec<String>, specializations: Option<Vec<String>> });
wire_struct!(InstallProgress {
    profile_id: String,
    bytes_pulled: i64,
    total_bytes: i64,
    done: bool
});
wire_struct!(InstallStatus {
    profile_id: String,
    bytes_pulled: i64,
    total_bytes: i64,
    bytes_per_second: i64,
    eta_seconds: i64,
    status: String,
    cancel_requested: bool
});
wire_struct!(UseCaseConflict {
    use_case: String,
    current_profile: String,
    new_profile: String
});

wire_reply!(List_Reply { profiles: Vec<ProfileInfo> });
wire_reply!(ListRuntimeManifests_Reply { runtimes: Vec<RuntimeManifestInfo> });
wire_reply!(ListRuntimeImages_Reply { images: Vec<OciRuntimeImage> });
wire_reply!(PruneUnusedRuntimeImages_Reply { removed: Vec<String>, errors: Vec<RuntimeImageCleanupError> });
wire_reply!(ListCatalog_Reply { profiles: Vec<CatalogProfileInfo> });
wire_reply!(ListInstalls_Reply { installs: Vec<InstallStatus> });
wire_reply!(InstallManifest_Reply { progress: InstallProgress, auto_assigned: Vec<String>, conflicts: Vec<UseCaseConflict> });
wire_reply!(InstallUrlProfile_Reply { progress: InstallProgress, auto_assigned: Vec<String>, conflicts: Vec<UseCaseConflict> });

pub const CUSTOM_TYPES: &[&zlink::idl::CustomType<'static>] = &[
    RuntimeImage::CUSTOM_TYPE,
    ProfileInfo::CUSTOM_TYPE,
    RuntimeManifestInfo::CUSTOM_TYPE,
    OciRuntimeImage::CUSTOM_TYPE,
    RuntimeImageCleanupError::CUSTOM_TYPE,
    UseCaseFitScore::CUSTOM_TYPE,
    FitScoreComponents::CUSTOM_TYPE,
    CatalogProfileInfo::CUSTOM_TYPE,
    InstallProgress::CUSTOM_TYPE,
    InstallStatus::CUSTOM_TYPE,
    UseCaseConflict::CUSTOM_TYPE,
];

#[derive(Clone, Debug, PartialEq, zlink::ReplyError, zlink::introspect::ReplyError)]
#[zlink(interface = "aileron.Models")]
pub enum Error {
    ProfileNotFound {
        profile_id: String,
    },
    ProfileInUse {
        profile_id: String,
    },
    InstallFailed {
        profile_id: String,
        reason: String,
    },
    UnsupportedUseCase {
        profile_id: String,
        use_case: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[zlink::proxy("aileron.Models")]
pub trait VarlinkClientInterface {
    async fn list(&mut self) -> zlink::Result<std::result::Result<List_Reply, Error>>;
    async fn list_runtime_manifests(
        &mut self,
    ) -> zlink::Result<std::result::Result<ListRuntimeManifests_Reply, Error>>;
    async fn list_runtime_images(
        &mut self,
    ) -> zlink::Result<std::result::Result<ListRuntimeImages_Reply, Error>>;
    async fn remove_runtime_image(
        &mut self,
        image_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
    async fn update_runtime_image(
        &mut self,
        image_ref: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
    async fn prune_unused_runtime_images(
        &mut self,
    ) -> zlink::Result<std::result::Result<PruneUnusedRuntimeImages_Reply, Error>>;
    async fn list_catalog(
        &mut self,
    ) -> zlink::Result<std::result::Result<ListCatalog_Reply, Error>>;
    async fn list_installs(
        &mut self,
    ) -> zlink::Result<std::result::Result<ListInstalls_Reply, Error>>;
    async fn cancel_install(
        &mut self,
        profile_id: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
    #[zlink(more)]
    async fn install_manifest(
        &mut self,
        profile_id: String,
    ) -> zlink::Result<
        impl zlink::futures_util::Stream<
            Item = zlink::Result<std::result::Result<InstallManifest_Reply, Error>>,
        >,
    >;
    #[zlink(more)]
    async fn install_url_profile(
        &mut self,
        runtime_id: String,
        url: String,
        sha256: String,
        mmproj_url: String,
        mmproj_sha256: String,
        use_cases: Vec<String>,
    ) -> zlink::Result<
        impl zlink::futures_util::Stream<
            Item = zlink::Result<std::result::Result<InstallUrlProfile_Reply, Error>>,
        >,
    >;
    async fn delete_profile(
        &mut self,
        profile_id: String,
        force: bool,
    ) -> zlink::Result<std::result::Result<(), Error>>;
    async fn assign_use_case(
        &mut self,
        profile_id: String,
        use_case: String,
    ) -> zlink::Result<std::result::Result<(), Error>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_profile_accepts_missing_optional_metadata() {
        let profile: CatalogProfileInfo = serde_json::from_value(serde_json::json!({
            "profile_id":"p", "model_id":"m", "llmfit_model_id":"", "runtime_id":"runtime",
            "tier":"balanced", "disk_size_gb":1.0, "min_ram_gb":1.0,
            "recommended_ram_gb":1.0, "min_vram_gb":0.0, "fit_score":0.0,
            "use_case_fit_scores":[], "fit_level":"recommended", "recommended":true,
            "installing":false, "recommendation_reason":"test", "use_cases":[]
        }))
        .unwrap();
        assert_eq!(profile.spdx_license, None);
        assert_eq!(profile.score_components, None);
        assert_eq!(profile.specializations, None);
    }

    #[test]
    fn install_error_preserves_typed_parameters() {
        let error = Error::InstallFailed {
            profile_id: "p".into(),
            reason: "digest mismatch".into(),
        };
        let decoded: Error = serde_json::from_value(serde_json::to_value(&error).unwrap()).unwrap();
        assert_eq!(decoded, error);
    }

    #[test]
    fn every_declared_error_round_trips_with_its_qualified_name() {
        let errors = [
            Error::ProfileNotFound {
                profile_id: "p".into(),
            },
            Error::ProfileInUse {
                profile_id: "p".into(),
            },
            Error::InstallFailed {
                profile_id: "p".into(),
                reason: "failed".into(),
            },
            Error::UnsupportedUseCase {
                profile_id: "p".into(),
                use_case: "case".into(),
            },
        ];
        let names = [
            "ProfileNotFound",
            "ProfileInUse",
            "InstallFailed",
            "UnsupportedUseCase",
        ];

        for (error, name) in errors.into_iter().zip(names) {
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["error"], format!("aileron.Models.{name}"));
            assert!(value["parameters"].is_object());
            assert_eq!(serde_json::from_value::<Error>(value).unwrap(), error);
        }
    }
}
