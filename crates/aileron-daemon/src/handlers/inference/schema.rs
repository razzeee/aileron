//! The public structured-output subset is deliberately smaller than JSON Schema.
use super::*;

pub(super) fn response_schema(
    fields: Option<&[GuidedField]>,
    explicit: Option<&str>,
) -> Result<Option<Value>, String> {
    let Some(explicit) = explicit else {
        return fields.map(guided_fields_schema).transpose();
    };
    let fields = fields.ok_or("response_schema requires guided generation")?;
    if !fields.is_empty() {
        return Err("response_schema and fields are mutually exclusive".into());
    }
    if explicit.len() > 64 * 1024 {
        return Err("response_schema exceeds 64 KiB".into());
    }
    let schema: Value = serde_json::from_str(explicit).map_err(|e| e.to_string())?;
    if schema["type"] != "object" {
        return Err("response_schema must have an object root".into());
    }
    validate(&schema, 0)?;
    Ok(Some(schema))
}

fn validate(schema: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err("response_schema nesting exceeds 16".into());
    }
    let object = schema.as_object().ok_or("schema must be an object")?;
    let kind = schema["type"].as_str().ok_or("schema type is required")?;
    let allowed: &[&str] = match kind {
        "object" => &["properties", "required", "additionalProperties"],
        "array" => &["items", "minItems", "maxItems"],
        "string" => &["enum", "minLength", "maxLength"],
        "number" | "integer" => &["enum", "minimum", "maximum"],
        "boolean" | "null" => &["enum"],
        _ => return Err(format!("unsupported schema type: {kind}")),
    };
    for key in object.keys() {
        if !matches!(key.as_str(), "type" | "description") && !allowed.contains(&key.as_str()) {
            return Err(format!("unsupported schema keyword for {kind}: {key}"));
        }
    }
    if object.get("description").is_some_and(|v| !v.is_string()) {
        return Err("description must be a string".into());
    }
    if kind == "object" {
        let props = schema["properties"]
            .as_object()
            .ok_or("object properties are required")?;
        for child in props.values() {
            validate(child, depth + 1)?;
        }
        if let Some(required) = object.get("required") {
            let required = required.as_array().ok_or("required must be an array")?;
            let mut seen = std::collections::HashSet::new();
            for name in required {
                let name = name.as_str().ok_or("required names must be strings")?;
                if !props.contains_key(name) || !seen.insert(name) {
                    return Err("required names must be unique declared properties".into());
                }
            }
        }
        if object
            .get("additionalProperties")
            .is_some_and(|v| !v.is_boolean())
        {
            return Err("additionalProperties must be boolean".into());
        }
    }
    if kind == "array" {
        validate(
            object.get("items").ok_or("array items are required")?,
            depth + 1,
        )?;
    }
    for (min, max, integer) in [
        ("minLength", "maxLength", true),
        ("minItems", "maxItems", true),
        ("minimum", "maximum", false),
    ] {
        for key in [min, max] {
            if let Some(value) = object.get(key) {
                if (integer && value.as_u64().is_none()) || (!integer && !value.is_number()) {
                    return Err(format!("invalid {key}"));
                }
            }
        }
        if let (Some(min), Some(max)) = (schema[min].as_f64(), schema[max].as_f64()) {
            if min > max {
                return Err("schema minimum exceeds maximum".into());
            }
        }
    }
    if let Some(values) = object.get("enum") {
        let values = values
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or("enum must be nonempty")?;
        let mut without_enum = schema.clone();
        without_enum.as_object_mut().unwrap().remove("enum");
        for value in values {
            crate::container::validate_json_schema(&value.to_string(), &without_enum)
                .map_err(|e| format!("invalid enum member: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_constraints_validate_outputs_and_reject_unsupported_schemas() {
        let schema = response_schema(Some(&[]), Some(r#"{"type":"object","properties":{"category":{"type":"string","enum":["work","personal"]},"scores":{"type":"array","items":{"type":"integer","minimum":0,"maximum":5},"maxItems":2},"ok":{"type":"boolean","enum":[true]}},"required":["category","ok"],"additionalProperties":false}"#)).unwrap().unwrap();
        assert!(
            crate::container::validate_json_schema(
                r#"{"category":"work","scores":[5],"ok":true}"#,
                &schema
            )
            .is_ok()
        );
        for output in [
            r#"{"category":"other","ok":true}"#,
            r#"{"category":"work","scores":[6],"ok":true}"#,
            r#"{"category":"work","ok":false}"#,
        ] {
            assert!(crate::container::validate_json_schema(output, &schema).is_err());
        }
        for schema in [
            r#"{"type":"object","properties":{"unused":{"type":"string","pattern":"x"}}}"#,
            r#"{"type":"object","properties":{},"required":["missing"]}"#,
            r#"{"type":"object","properties":{},"allOf":[]}"#,
        ] {
            assert!(response_schema(Some(&[]), Some(schema)).is_err());
        }
        assert!(response_schema(None, Some("{}")).is_err());
    }
}
