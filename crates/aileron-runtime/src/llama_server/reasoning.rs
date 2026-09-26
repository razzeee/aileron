use crate::Request;
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Debug, Clone, Default)]
pub(super) struct Capabilities {
    pub thinking_modes: Vec<&'static str>,
    pub efforts: Vec<&'static str>,
    legacy_off: bool,
    qwen: bool,
    gemma: bool,
    gpt_oss: bool,
    deepseek: bool,
}

impl Capabilities {
    pub fn from_props(props: &Value, model_name: &str) -> Self {
        let template = props["chat_template"].as_str().unwrap_or_default();
        let name = model_name.to_ascii_lowercase().replace([' ', '_'], "-");
        // Restrict mode/effort claims to known template families; the server's
        // generic variable probe alone does not establish trained effort levels.
        if name.contains("gemma")
            && template.contains("<|channel>thought")
            && template.contains("enable_thinking")
        {
            Self {
                thinking_modes: vec!["auto", "on", "off"],
                legacy_off: true,
                gemma: true,
                ..Self::default()
            }
        } else if name.contains("gpt-oss")
            && template.contains("Reasoning:")
            && template.contains("<|start|>")
            && props["chat_template_caps"]
                .get("supports_reasoning_effort")
                .map_or_else(
                    // Older servers render this variable but do not expose a
                    // capability probe for it. Limit the fallback to GPT-OSS's
                    // recognized Harmony template, never arbitrary templates.
                    || template.contains("reasoning_effort"),
                    |supported| supported == true,
                )
        {
            Self {
                thinking_modes: vec!["auto", "on"],
                efforts: vec!["low", "medium", "high"],
                gpt_oss: true,
                ..Self::default()
            }
        } else if name.contains("deepseek")
            && name.contains("r1")
            && (template.contains("<think>") || template.contains("</think>"))
        {
            Self {
                thinking_modes: vec!["auto", "on"],
                deepseek: true,
                ..Self::default()
            }
        } else if name.contains("qwen3")
            && template.contains("<|im_start|>")
            && template.contains("<think>")
        {
            Self {
                thinking_modes: if template.contains("enable_thinking")
                    && !name.contains("deepseek")
                    && !name.contains("thinking")
                {
                    vec!["auto", "on", "off"]
                } else {
                    vec!["auto", "on"]
                },
                qwen: !name.contains("qwen3.5"),
                ..Self::default()
            }
        } else {
            Self::default()
        }
    }

    pub fn json(&self) -> Value {
        json!({"thinking_modes":self.thinking_modes, "reasoning_efforts":self.efforts,
            "reasoning_output":!self.thinking_modes.is_empty()})
    }

    pub fn apply(&self, req: &Request, body: &mut Value) -> Result<()> {
        let mode = req.thinking.as_deref().unwrap_or("auto");
        ensure!(
            matches!(mode, "auto" | "on" | "off"),
            "unknown thinking mode"
        );
        ensure!(
            mode == "auto" || self.thinking_modes.contains(&mode),
            "thinking mode is unsupported by this template"
        );
        ensure!(
            !req.include_reasoning || !self.thinking_modes.is_empty(),
            "reasoning output is unsupported by this template"
        );
        if let Some(effort) = &req.reasoning_effort {
            ensure!(
                mode != "off",
                "reasoning effort conflicts with thinking off"
            );
            ensure!(
                self.efforts.contains(&effort.as_str()),
                "reasoning effort is unsupported by this template"
            );
            body["reasoning_effort"] = json!(effort);
        }
        if mode != "auto" {
            body["chat_template_kwargs"]["enable_thinking"] = json!(mode == "on");
        } else if req.thinking.is_none() && self.legacy_off {
            // Gemma's native template path omitted the thinking prefix. Keep
            // existing calls usable with their existing output-token budgets.
            body["chat_template_kwargs"]["enable_thinking"] = json!(false);
        }
        let model_defaults = req.temperature.is_none()
            && !self.thinking_modes.is_empty()
            && (req.thinking.is_some() || req.reasoning_effort.is_some());
        if model_defaults {
            body.as_object_mut().unwrap().remove("temperature");
        }
        if model_defaults && self.qwen {
            body["temperature"] = json!(if mode == "off" { 0.7 } else { 0.6 });
            body["top_k"] = json!(20);
            body["top_p"] = json!(if mode == "off" { 0.8 } else { 0.95 });
        }
        if model_defaults && self.gemma {
            // Google's Gemma 4 model card recommends these across use cases.
            body["temperature"] = json!(1.0);
            body["top_p"] = json!(0.95);
            body["top_k"] = json!(64);
        }
        if model_defaults && self.gpt_oss {
            // The published GPT-OSS generation config enables sampling and
            // inherits these Transformers generation defaults.
            body["temperature"] = json!(1.0);
            body["top_p"] = json!(1.0);
            body["top_k"] = json!(50);
        }
        if model_defaults && self.deepseek {
            body["temperature"] = json!(0.6);
            body["top_p"] = json!(0.95);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_oss_effort_support_handles_old_props_without_overriding_a_denial() {
        let mut props = json!({"chat_template":"<|start|> Reasoning: {{ reasoning_effort }}"});
        assert_eq!(
            Capabilities::from_props(&props, "GPT-OSS-20B").efforts,
            ["low", "medium", "high"]
        );
        assert!(
            Capabilities::from_props(&props, "unknown")
                .efforts
                .is_empty()
        );
        props["chat_template_caps"] = json!({"supports_reasoning_effort":false});
        assert!(
            Capabilities::from_props(&props, "GPT-OSS-20B")
                .efforts
                .is_empty()
        );
    }

    #[test]
    fn effort_is_not_invented_for_hybrid_templates() {
        let caps = Capabilities::from_props(
            &json!({"chat_template":"<|im_start|><think> enable_thinking"}),
            "Qwen3-0.6B",
        );
        let req: Request =
            serde_json::from_value(json!({"thinking":"on","reasoning_effort":"high"})).unwrap();
        assert!(caps.apply(&req, &mut json!({})).is_err());
    }

    #[test]
    fn gemma_legacy_default_and_explicit_auto_are_distinct() {
        let caps = Capabilities::from_props(
            &json!({"chat_template":"<|channel>thought enable_thinking"}),
            "Gemma-4",
        );
        let req: Request = serde_json::from_value(json!({})).unwrap();
        let mut body = json!({});
        caps.apply(&req, &mut body).unwrap();
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        let req: Request = serde_json::from_value(json!({"thinking":"auto"})).unwrap();
        let mut body = json!({});
        caps.apply(&req, &mut body).unwrap();
        assert!(body.get("chat_template_kwargs").is_none());
    }
}
