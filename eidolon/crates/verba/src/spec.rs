//! A `templates.json` skeleton from tool manifests — the input to
//! `verba-volantia gen` for a `tools` bundle.
//!
//! The vault's bet is that training data "falls out of the manifest
//! fields". Half of it does: the function list, its parameters (required
//! first), and a starter template per tool from its description. The other
//! half — dozens of natural phrasings per function, which is what makes
//! dev/test off-template — still has to be authored (by hand or by a model
//! prompted with this skeleton). This produces the frame to fill.

use serde_json::{Value, json};

use eidolon_core::tool::ToolManifest;

pub const WRAPPERS: &[&str] = &[
    "{}",
    "please {}",
    "can you {}",
    "{} for me",
    "go ahead and {}",
    "i need you to {}",
    "{} now",
];

pub fn templates_skeleton(manifests: &[ToolManifest]) -> Value {
    let functions: Vec<Value> = manifests
        .iter()
        .map(|m| {
            let props = m.input_schema.get("properties").and_then(Value::as_object);
            let required: Vec<String> = m
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let mut params = required.clone();
            if let Some(p) = props {
                for k in p.keys() {
                    if !params.contains(k) {
                        params.push(k.clone());
                    }
                }
            }
            // One starter phrasing: the verb from the description plus every
            // required slot, e.g. "read {path}".
            let verb = m
                .description
                .split_whitespace()
                .next()
                .unwrap_or(&m.name)
                .to_lowercase();
            let slots: Vec<String> = required.iter().map(|r| format!("{{{r}}}")).collect();
            let starter = if slots.is_empty() {
                verb.clone()
            } else {
                format!("{verb} {}", slots.join(" "))
            };
            json!({
                "name": m.name,
                "params": params,
                "templates": [starter],
                "_description": m.description,
            })
        })
        .collect();
    json!({ "wrappers": WRAPPERS, "functions": functions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::policy::Approval;

    #[test]
    fn skeleton_lists_required_params_first() {
        let m = ToolManifest {
            name: "edit".into(),
            description: "Replace an exact substring in a file.".into(),
            input_schema: json!({ "type": "object", "properties": { "replace_all": {}, "path": {}, "old_str": {}, "new_str": {} }, "required": ["path", "old_str", "new_str"] }),
            approval: Approval::Mutating,
            prompt: None,
            render: None,
            deferred: false,
        };
        let s = templates_skeleton(&[m]);
        let f = &s["functions"][0];
        assert_eq!(
            f["params"],
            json!(["path", "old_str", "new_str", "replace_all"])
        );
        assert_eq!(f["templates"][0], "replace {path} {old_str} {new_str}");
    }
}
