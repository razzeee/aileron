//! Minimal adapter for llmfit's embedded Hugging Face model metadata.
//!
//! Aileron still treats model manifests as the installable source of truth. The
//! llmfit is only used to enrich catalog rows with fit and capability metadata
//! keyed by `llmfit_model_id`.

use std::collections::HashMap;
use std::sync::OnceLock;

use llmfit_core::{LlmModel, ModelDatabase, ModelFit, ModelFormat, RunMode, SystemSpecs};

static DATABASE: OnceLock<ModelDatabase> = OnceLock::new();

pub fn find(model_id: &str) -> Option<&'static LlmModel> {
    let database = DATABASE.get_or_init(ModelDatabase::embedded);
    database
        .get_all_models()
        .iter()
        .find(|model| model.name == model_id)
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
    if fit.effective_context_length > 0 {
        runtime_options
            .entry("N_CTX".to_string())
            .or_insert_with(|| fit.effective_context_length.to_string());
    }
    if fit.run_mode == RunMode::CpuOnly {
        runtime_options
            .entry("N_GPU_LAYERS".to_string())
            .or_insert_with(|| "0".to_string());
    }
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
}
