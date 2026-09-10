use aileron_varlink::{INTERFACES, inference, models, permissions, sessions};
use zlink::introspect::ReplyError as _;

#[test]
fn generated_introspection_matches_idl_fields_and_errors() {
    for ((name, source), (types, errors)) in INTERFACES.into_iter().zip([
        (inference::CUSTOM_TYPES, inference::Error::VARIANTS),
        (models::CUSTOM_TYPES, models::Error::VARIANTS),
        (permissions::CUSTOM_TYPES, permissions::Error::VARIANTS),
        (sessions::CUSTOM_TYPES, sessions::Error::VARIANTS),
    ]) {
        let idl = zlink::idl::Interface::try_from(source).unwrap();
        assert_eq!(idl.name(), name);
        assert_eq!(idl.custom_types().count(), types.len());
        for rust_type in types {
            let idl_type = idl
                .custom_types()
                .find(|ty| ty.name() == rust_type.name())
                .unwrap();
            let fields = |object: &zlink::idl::CustomObject<'_>| {
                object
                    .fields()
                    .map(|field| (field.name().to_string(), field.ty().to_string()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                fields(rust_type.as_object().unwrap()),
                fields(idl_type.as_object().unwrap()),
                "{name}.{}",
                rust_type.name()
            );
        }
        assert_eq!(idl.errors().count(), errors.len());
        for rust_error in errors {
            let idl_error = idl
                .errors()
                .find(|error| error.name() == rust_error.name())
                .unwrap();
            assert_eq!(
                rust_error
                    .fields()
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>(),
                idl_error
                    .fields()
                    .map(|field| (field.name(), field.ty()))
                    .collect::<Vec<_>>(),
                "{name}.{}",
                rust_error.name(),
            );
        }
    }
}

#[test]
fn catalog_profile_accepts_missing_optional_metadata() {
    let profile: models::CatalogProfileInfo = serde_json::from_value(serde_json::json!({
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
fn generated_stream_cursor_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<inference::InferenceReplyStream<inference::StreamResponse_Reply>>();
}
