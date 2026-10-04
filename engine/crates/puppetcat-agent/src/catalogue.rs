// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The effect, filter, audio and transition catalogues as an agent reads them.
//!
//! The window shows these as shelves of cards; this is the same list as data,
//! with the parameters each one takes, so an agent can pick an id and set it on
//! a clip without guessing.

use puppetcat_effects::{Catalogue, Kind, Package, ParamType, TRANSITIONS};
use serde_json::{Value, json};

/// Which part of the catalogue a caller wants.
pub fn parse_kind(name: &str) -> Result<&'static str, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "effect" | "effects" | "video" => Ok("effects"),
        "filter" | "filters" => Ok("filters"),
        "audio" | "audio-effects" => Ok("audio"),
        "transition" | "transitions" => Ok("transitions"),
        other => Err(format!(
            "unknown kind {other:?}; use effects, filters, audio or transitions"
        )),
    }
}

fn describe(package: &Package, include_params: bool) -> Value {
    let meta = &package.manifest.effect;
    let mut entry = json!({
        "id": meta.id,
        "name": meta.name,
        "category": meta.category,
        "description": meta.description,
    });
    if let Some(intensity) = &meta.intensity {
        entry["intensityParam"] = json!(intensity);
    }
    if include_params {
        entry["params"] = package
            .manifest
            .params
            .iter()
            .map(|param| {
                let kind = match param.kind {
                    ParamType::Float => "float",
                    ParamType::Int => "int",
                    ParamType::Bool => "bool",
                    ParamType::Enum => "enum",
                    ParamType::Color => "color",
                    _ => "other",
                };
                let mut value = json!({
                    "key": param.key,
                    "label": param.label,
                    "type": kind,
                    "min": param.min,
                    "max": param.max,
                    "default": param.default,
                    "step": param.step,
                    "unit": param.unit,
                });
                if !param.values.is_empty() {
                    value["values"] = json!(param.values);
                    value["labels"] = json!(param.labels);
                }
                value
            })
            .collect();
    }
    entry
}

/// The catalogue as JSON: one array per kind asked for, every kind when none
/// is. Within a kind, in the order the window lists them.
pub fn list(kind: Option<&str>, include_params: bool) -> Result<Value, String> {
    let wanted = kind.map(parse_kind).transpose()?;
    let catalogue = Catalogue::builtin();
    let shelf = |kind: Kind| -> Vec<Value> {
        catalogue
            .of_kind(kind)
            .map(|package| describe(package, include_params))
            .collect()
    };
    let mut out = serde_json::Map::new();
    let mut put = |name: &str, build: &dyn Fn() -> Value| {
        if wanted.is_none_or(|w| w == name) {
            out.insert(name.to_owned(), build());
        }
    };
    put("effects", &|| json!(shelf(Kind::Effect)));
    put("filters", &|| json!(shelf(Kind::Filter)));
    put("audio", &|| json!(shelf(Kind::Audio)));
    put("transitions", &|| {
        TRANSITIONS
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "category": t.category,
                    "description": t.description,
                })
            })
            .collect::<Vec<_>>()
            .into()
    });
    out.insert(
        "usage".to_owned(),
        json!({
            "effects": "update_clip patch.videoEffects: [{\"id\", \"params\": {key: value}, \"enabled\": true}]",
            "filters": "same field as effects: patch.videoEffects",
            "audio": "update_clip patch.filters: [{\"id\", \"params\", \"enabled\"}]",
            "transitions": "update_clip patch.transitionIn: {\"id\", \"duration\"} on the clip the cut leads into; null clears it",
        }),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_kind_with_parameters_and_filters_by_kind() {
        let all = list(None, true).expect("lists");
        assert!(all["effects"].as_array().is_some_and(|a| !a.is_empty()));
        assert!(all["audio"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(all["transitions"].as_array().map(Vec::len), Some(7));
        let green = all["effects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == "concat.green-screen")
            .expect("green screen is an effect");
        assert_eq!(green["params"][0]["key"], "similarity");

        let only = list(Some("transitions"), false).expect("lists");
        assert!(only.get("effects").is_none());
        assert!(only["transitions"][0].get("params").is_none());
        assert!(list(Some("nonsense"), true).is_err());
    }
}
