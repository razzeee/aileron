//! Minimal adapter for llmfit's embedded Hugging Face model metadata.
//!
//! Aileron still treats model manifests as the installable source of truth. The
//! llmfit is only used to enrich catalog rows with fit and capability metadata
//! keyed by `llmfit_model_id`.

use std::sync::OnceLock;

use llmfit_core::{LlmModel, ModelDatabase, ModelFit, SystemSpecs};

static DATABASE: OnceLock<ModelDatabase> = OnceLock::new();

pub fn find(model_id: &str) -> Option<&'static LlmModel> {
    let database = DATABASE.get_or_init(ModelDatabase::embedded);
    database
        .get_all_models()
        .iter()
        .find(|model| model.name == model_id)
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
