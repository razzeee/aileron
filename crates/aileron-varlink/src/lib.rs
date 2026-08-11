//! Owned zlink contracts for Aileron's four Varlink interfaces.
//!
//! The checked-in `.varlink` files are the wire-format source of truth. These
//! bindings are handwritten so streaming replies remain owned and reviewable.

#[allow(non_snake_case)]
pub mod aileron_Inference;
#[allow(non_snake_case)]
pub mod aileron_Models;
#[allow(non_snake_case)]
pub mod aileron_Permissions;
#[allow(non_snake_case)]
pub mod aileron_Sessions;

pub use aileron_Inference as inference;
pub use aileron_Models as models;
pub use aileron_Permissions as permissions;
pub use aileron_Sessions as sessions;

/// The protocol IDLs embedded for service introspection and drift tests.
pub const INTERFACES: [(&str, &str); 4] = [
    (
        "aileron.Inference",
        include_str!("../varlink/aileron.Inference.varlink"),
    ),
    (
        "aileron.Models",
        include_str!("../varlink/aileron.Models.varlink"),
    ),
    (
        "aileron.Permissions",
        include_str!("../varlink/aileron.Permissions.varlink"),
    ),
    (
        "aileron.Sessions",
        include_str!("../varlink/aileron.Sessions.varlink"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use zlink::introspect::ReplyError as _;

    #[test]
    fn every_checked_in_idl_parses_and_has_the_expected_name() {
        for (name, source) in INTERFACES {
            let interface = zlink::idl::Interface::try_from(source).expect("valid Varlink IDL");
            assert_eq!(interface.name(), name);
            assert!(!interface.is_empty());
        }
    }

    #[test]
    fn handwritten_custom_types_and_errors_match_the_idls_structurally() {
        assert_contract(
            INTERFACES[0].1,
            inference::CUSTOM_TYPES,
            inference::Error::VARIANTS,
            &[
                "GetUseCaseAvailability",
                "CreateSession",
                "Prewarm",
                "StreamResponse",
                "StreamRespondGuided",
                "StreamSubmitToolResultsGuided",
                "StreamEmbed",
                "StreamTranscribe",
                "StreamSynthesize",
                "StreamDescribe",
                "StreamOcr",
                "StreamDetect",
                "StreamSegment",
                "StreamDepth",
                "CancelActiveRequest",
                "EndSession",
            ],
        );
        assert_contract(
            INTERFACES[1].1,
            models::CUSTOM_TYPES,
            models::Error::VARIANTS,
            &[
                "List",
                "ListRuntimeManifests",
                "ListRuntimeImages",
                "RemoveRuntimeImage",
                "UpdateRuntimeImage",
                "PruneUnusedRuntimeImages",
                "ListCatalog",
                "ListInstalls",
                "CancelInstall",
                "InstallManifest",
                "InstallUrlProfile",
                "DeleteProfile",
                "AssignUseCase",
            ],
        );
        assert_contract(
            INTERFACES[2].1,
            permissions::CUSTOM_TYPES,
            permissions::Error::VARIANTS,
            &["ListAppPermissions", "SetAppPermission"],
        );
        assert_contract(
            INTERFACES[3].1,
            sessions::CUSTOM_TYPES,
            sessions::Error::VARIANTS,
            &["ListActive", "KillSession"],
        );
    }

    fn assert_contract(
        source: &str,
        rust_types: &[&zlink::idl::CustomType<'static>],
        rust_errors: &[&zlink::idl::Error<'static>],
        rust_methods: &[&str],
    ) {
        let idl = zlink::idl::Interface::try_from(source).unwrap();
        let idl_methods = idl
            .methods()
            .map(|method| method.name())
            .collect::<Vec<_>>();
        assert_eq!(
            idl_methods,
            rust_methods,
            "method list drifted for {}",
            idl.name()
        );

        let idl_types = idl.custom_types().collect::<Vec<_>>();
        assert_eq!(
            idl_types.len(),
            rust_types.len(),
            "type count drifted for {}",
            idl.name()
        );
        for rust_type in rust_types {
            let idl_type = idl_types
                .iter()
                .find(|candidate| candidate.name() == rust_type.name())
                .unwrap_or_else(|| panic!("{} is absent from {}", rust_type.name(), idl.name()));
            let rust_object = rust_type
                .as_object()
                .expect("Aileron IDLs use object types");
            let idl_object = idl_type.as_object().expect("Aileron IDLs use object types");
            let rust_fields = rust_object
                .fields()
                .map(|field| (field.name(), field.ty()))
                .collect::<Vec<_>>();
            let idl_fields = idl_object
                .fields()
                .map(|field| (field.name(), field.ty()))
                .collect::<Vec<_>>();
            assert_eq!(
                rust_fields,
                idl_fields,
                "field drift in {}.{}",
                idl.name(),
                rust_type.name()
            );
        }

        let idl_errors = idl.errors().collect::<Vec<_>>();
        assert_eq!(
            idl_errors.len(),
            rust_errors.len(),
            "error count drifted for {}",
            idl.name()
        );
        for rust_error in rust_errors {
            let idl_error = idl_errors
                .iter()
                .find(|candidate| candidate.name() == rust_error.name())
                .unwrap_or_else(|| panic!("{} is absent from {}", rust_error.name(), idl.name()));
            let rust_fields = rust_error
                .fields()
                .map(|field| (field.name(), field.ty()))
                .collect::<Vec<_>>();
            let idl_fields = idl_error
                .fields()
                .map(|field| (field.name(), field.ty()))
                .collect::<Vec<_>>();
            assert_eq!(
                rust_fields,
                idl_fields,
                "field drift in {}.{}",
                idl.name(),
                rust_error.name()
            );
        }
    }

    #[test]
    fn aliases_keep_the_public_contract_modules_available() {
        let value = models::RuntimeImage {
            variant: "cpu".into(),
            image_ref: "example/image:latest".into(),
        };
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json["variant"], "cpu");

        let permission = permissions::AppPermission {
            app_id: "org.example.App".into(),
            use_case: "language.generate".into(),
            allowed: true,
            last_used: None,
        };
        assert!(serde_json::to_value(permission).unwrap()["last_used"].is_null());
    }
}
