//! Minimal adapter for llmfit's embedded Hugging Face model metadata.
//!
//! Aileron still treats model manifests as the installable source of truth. The
//! llmfit is only used to enrich catalog rows with fit and capability metadata
//! keyed by `llmfit_model_id`.

use std::collections::HashMap;
use std::sync::OnceLock;

use llmfit_core::{
    Capability, LlmModel, ModelDatabase, ModelFit, ModelFormat, RunMode, SystemSpecs,
};

pub const VITS_RUNTIME_ID: &str = "tts-vits";
pub const SUPPORTED_LANGUAGES_OPTION: &str = "SUPPORTED_LANGUAGES";

static DATABASE: OnceLock<ModelDatabase> = OnceLock::new();

pub fn find(model_id: &str) -> Option<&'static LlmModel> {
    let database = DATABASE.get_or_init(ModelDatabase::embedded);
    let models = database.get_all_models();
    models
        .iter()
        .find(|model| model.name == model_id)
        .or_else(|| {
            let model_slug = canonical_slug(model_id);
            models
                .iter()
                .find(|model| canonical_slug(&model.name) == model_slug)
        })
}

pub fn all() -> &'static [LlmModel] {
    let database = DATABASE.get_or_init(ModelDatabase::embedded);
    database.get_all_models().as_slice()
}

pub fn detect_system() -> SystemSpecs {
    SystemSpecs::detect()
}

pub fn fit_score(model: &LlmModel, system: &SystemSpecs) -> f64 {
    ModelFit::analyze(model, system).score
}

pub fn fit_score_for_category(model: &LlmModel, system: &SystemSpecs, category: &str) -> f64 {
    let mut model = model.clone();
    model.use_case = category.to_string();
    ModelFit::analyze(&model, system).score
}

pub fn is_supported_vits_tts(model: &LlmModel) -> bool {
    model.format == ModelFormat::Safetensors
        && model
            .architecture
            .as_deref()
            .is_some_and(|architecture| architecture.eq_ignore_ascii_case("vits"))
        && Capability::infer(model).contains(&Capability::Tts)
}

pub fn supported_languages_option(model: &LlmModel) -> Option<String> {
    let mut languages = model
        .languages
        .iter()
        .map(|language| language.trim().to_ascii_lowercase())
        .filter(|language| !language.is_empty())
        .collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    (!languages.is_empty()).then(|| languages.join(","))
}

pub fn apply_llama_runtime_options(
    model_id: &str,
    runtime_options: &mut HashMap<String, String>,
    system: &SystemSpecs,
) {
    if model_id.trim().is_empty() {
        return;
    }
    let Some(model) = find(model_id) else {
        return;
    };
    if model.format != ModelFormat::Gguf {
        return;
    }

    let fit = ModelFit::analyze(model, system);
    if fit.usable_context > 0 {
        runtime_options
            .entry("N_CTX".to_string())
            .or_insert_with(|| fit.usable_context.to_string());
    }
    if fit.run_mode == RunMode::CpuOnly {
        runtime_options
            .entry("N_GPU_LAYERS".to_string())
            .or_insert_with(|| "0".to_string());
    }
}

fn canonical_slug(name: &str) -> String {
    name.split('/')
        .next_back()
        .unwrap_or(name)
        .to_ascii_lowercase()
        .replace(['-', '_', '.'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_metadata_by_name() {
        let model = find("brain-bzh/reve-positions").expect("metadata exists");

        assert_eq!(model.min_ram_gb, 1.0);
        assert_eq!(model.recommended_ram_gb, 2.0);
    }

    #[test]
    fn finds_metadata_by_canonical_slug() {
        let model = find("google/gemma-4-E4B-it").expect("deduplicated metadata exists");

        assert_eq!(canonical_slug(&model.name), "gemma4e4bit");
    }

    #[test]
    fn finds_supported_vits_tts_metadata() {
        let model = all()
            .iter()
            .find(|model| is_supported_vits_tts(model))
            .expect("llmfit should include a supported VITS model");

        assert_eq!(model.format, ModelFormat::Safetensors);
        assert!(
            model
                .architecture
                .as_deref()
                .is_some_and(|architecture| architecture.eq_ignore_ascii_case("vits"))
        );
        assert!(Capability::infer(model).contains(&Capability::Tts));
    }

    #[test]
    fn supported_languages_are_normalized_and_stable() {
        let model = all()
            .iter()
            .find(|model| is_supported_vits_tts(model) && !model.languages.is_empty())
            .expect("supported VITS language metadata exists");
        let option = supported_languages_option(model).expect("language option");
        let values = option.split(',').collect::<Vec<_>>();

        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            values
                .iter()
                .all(|value| *value == value.to_ascii_lowercase())
        );
    }
}
