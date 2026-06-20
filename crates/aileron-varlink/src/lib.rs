/// Varlink interface definitions and generated type bindings for the Aileron project.
///
/// The four interfaces are:
/// - `aileron.Inference`   – create sessions, generate text, transcribe audio, describe images
/// - `aileron.Models`      – list, pull, delete, and assign OCI images
/// - `aileron.Permissions` – per-app, per-use-case permission records
/// - `aileron.Sessions`    – inspect and kill active inference sessions
///
/// Generated source files are produced by `varlink_generator::cargo_build` and
/// placed in `$OUT_DIR`.  The module name matches the file stem produced by the
/// generator (dots in the interface name are replaced with underscores).

// The varlink code generator produces non-standard names (e.g. `Call_Foo`,
// `Foo_Args`).  Suppress the relevant lints for these modules only.
#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Inference {
    include!(concat!(env!("OUT_DIR"), "/aileron.Inference.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Models {
    include!(concat!(env!("OUT_DIR"), "/aileron.Models.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Permissions {
    include!(concat!(env!("OUT_DIR"), "/aileron.Permissions.rs"));
}

#[allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    unused_imports,
    clippy::all
)]
pub mod aileron_Sessions {
    include!(concat!(env!("OUT_DIR"), "/aileron.Sessions.rs"));
}

// Convenience aliases that downstream crates use.
pub use aileron_Inference as inference;
pub use aileron_Models as models;
pub use aileron_Permissions as permissions;
pub use aileron_Sessions as sessions;

#[cfg(test)]
mod tests {
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn catalog_profile_info_accepts_missing_license() {
        let profile: crate::aileron_Models::CatalogProfileInfo =
            serde_json::from_value(serde_json::json!({
                "profile_id": "old-daemon-profile",
                "model_id": "old-daemon-model",
                "llmfit_model_id": "",
                "runtime_id": "llm-llama-cpp",
                "tier": "balanced",
                "disk_size_gb": 1.0,
                "min_ram_gb": 1.0,
                "recommended_ram_gb": 1.0,
                "min_vram_gb": 0.0,
                "fit_score": 0.0,
                "use_case_fit_scores": [],
                "fit_level": "recommended",
                "recommended": true,
                "installing": false,
                "recommendation_reason": "test",
                "use_cases": ["language.extract"]
            }))
            .expect("missing spdx_license should decode from older daemons");

        assert_eq!(profile.spdx_license, None);
    }

    #[hegel::test]
    fn catalog_profile_info_decodes_optional_license(tc: TestCase) {
        let include_license = tc.draw(gs::booleans());
        let license = tc.draw(gs::sampled_from(vec![
            "MIT".to_string(),
            "Apache-2.0".to_string(),
            "GPL-3.0-or-later".to_string(),
        ]));
        let mut value = serde_json::json!({
            "profile_id": "profile",
            "model_id": "model",
            "llmfit_model_id": "",
            "runtime_id": "llm-llama-cpp",
            "tier": "balanced",
            "disk_size_gb": 1.0,
            "min_ram_gb": 1.0,
            "recommended_ram_gb": 1.0,
            "min_vram_gb": 0.0,
            "fit_score": 0.0,
            "use_case_fit_scores": [],
            "fit_level": "recommended",
            "recommended": true,
            "installing": false,
            "recommendation_reason": "test",
            "use_cases": ["language.extract"]
        });
        if include_license {
            value["spdx_license"] = serde_json::Value::String(license.clone());
        }

        let profile: crate::aileron_Models::CatalogProfileInfo =
            serde_json::from_value(value).expect("catalog profile should decode");

        assert_eq!(
            profile.spdx_license.as_deref(),
            include_license.then_some(license.as_str())
        );
    }
}
