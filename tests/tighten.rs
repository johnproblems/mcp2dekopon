#![allow(clippy::unwrap_used)]

//! Golden tests for the schema tightener: one rule per snapshot.

use std::collections::HashSet;

use mcp2dekopon::fetch::McpTool;
use mcp2dekopon::tighten::{tighten_tool, TightenedTool};
use serde_json::{json, Value};

#[path = "snap.rs"]
mod snap;

fn tool(name: &str, schema: Value) -> McpTool {
    McpTool {
        name: name.to_string(),
        description: Some(format!("Tool {name}")),
        input_schema: schema,
        read_only: None,
        destructive: None,
    }
}

fn tighten(schema: Value) -> Result<TightenedTool, String> {
    tighten_one(tool("probe", schema))
}

fn tighten_named(name: &str, schema: Value) -> Result<TightenedTool, String> {
    tighten_one(tool(name, schema))
}

fn tighten_one(tool: McpTool) -> Result<TightenedTool, String> {
    let mut names = HashSet::new();
    let mut types = HashSet::new();
    tighten_tool(&tool, &mut names, 200, &mut types)
        .map_err(|skip| format!("{}: {}", skip.reason, skip.detail))
}

fn snapshot(name: &str, tightened: &TightenedTool) {
    let mut rendered = serde_json::to_string_pretty(tightened).unwrap();
    rendered.push('\n');
    snap::assert_snapshot(name, &rendered);
}

#[test]
fn scalars_with_requiredness() {
    let schema = json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "The question."},
            "count": {"type": "integer"},
            "ratio": {"type": "number"},
            "dryRun": {"type": "boolean"},
            "mode": {"type": "string", "enum": ["fast", "slow"]},
        },
        "required": ["query", "count"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("enum-not-constrained"));
    snapshot("tighten_scalars", &tightened);
}

#[test]
fn nested_objects_and_arrays() {
    let schema = json!({
        "type": "object",
        "properties": {
            "filter": {
                "type": "object",
                "properties": {
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "limit": {"type": "integer"},
                },
                "required": ["tags"],
            },
            "matrix": {"type": "array", "items": {"type": "array", "items": {"type": "number"}}},
        },
        "required": ["filter"],
    });
    snapshot("tighten_nested", &tighten(schema).unwrap());
}

#[test]
fn free_map_becomes_json_string() {
    let schema = json!({
        "type": "object",
        "properties": {
            "labels": {"type": "object", "additionalProperties": {"type": "string"}},
        },
        "required": ["labels"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.flags.contains("free-map-as-string"));
    snapshot("tighten_free_map", &tightened);
}

#[test]
fn top_level_map_becomes_payload() {
    let schema = json!({
        "type": "object",
        "additionalProperties": {"type": "string"},
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.top_level_map);
    assert!(tightened.flags.contains("free-map-as-string"));
    snapshot("tighten_top_level_map", &tightened);
}

#[test]
fn refs_inline_from_defs() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "Filter": {
                "type": "object",
                "properties": {"q": {"type": "string"}},
                "required": ["q"],
            },
        },
        "properties": {
            "filter": {"$ref": "#/$defs/Filter"},
        },
        "required": ["filter"],
    });
    snapshot("tighten_refs", &tighten(schema).unwrap());
}

#[test]
fn recursive_ref_skips() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {"child": {"$ref": "#/$defs/Node"}},
            },
        },
        "properties": {
            "node": {"$ref": "#/$defs/Node"},
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("recursive-schema"), "{error}");
}

#[test]
fn external_ref_skips() {
    let schema = json!({
        "type": "object",
        "properties": {
            "node": {"$ref": "https://example.test/schema.json#/Node"},
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("external-ref"), "{error}");
}

#[test]
fn unresolved_ref_skips() {
    let schema = json!({
        "type": "object",
        "properties": {
            "node": {"$ref": "#/$defs/Missing"},
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("unresolved-ref"), "{error}");
}

#[test]
fn oneof_objects_becomes_enum() {
    let schema = json!({
        "type": "object",
        "properties": {
            "target": {
                "oneOf": [
                    {"title": "Repo", "type": "object", "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
                    {"title": "Gist", "type": "object", "properties": {"gist": {"type": "string"}}},
                ],
            },
        },
        "required": ["target"],
    });
    snapshot("tighten_enum", &tighten(schema).unwrap());
}

#[test]
fn nullable_collapses_to_optional() {
    let schema = json!({
        "type": "object",
        "properties": {
            "a": {"anyOf": [{"type": "string"}, {"type": "null"}]},
            "b": {"type": ["integer", "null"]},
        },
        "required": ["a", "b"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.fields.iter().all(|f| !f.required));
    snapshot("tighten_nullable", &tightened);
}

#[test]
fn scalar_union_lowers_to_enum() {
    let schema = json!({
        "type": "object",
        "properties": {
            "repo": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
            "count": {"oneOf": [{"type": "integer"}, {"type": "number"}]},
        },
        "required": ["repo"],
    });
    snapshot("tighten_scalar_union", &tighten(schema).unwrap());
}

#[test]
fn mixed_struct_scalar_union_skips() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": {
                "oneOf": [
                    {"type": "string"},
                    {"type": "object", "properties": {"a": {"type": "string"}}},
                ],
            },
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("unsupported-union"), "{error}");
}

#[test]
fn nested_array_union_skips() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": {
                "oneOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"type": "object", "properties": {"a": {"type": "string"}}}},
                ],
            },
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("unsupported-union"), "{error}");
}

#[test]
fn allof_merges_plain_objects() {
    let schema = json!({
        "type": "object",
        "properties": {
            "both": {
                "allOf": [
                    {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                    {"type": "object", "properties": {"b": {"type": "integer"}}},
                ],
            },
        },
        "required": ["both"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.flags.contains("allof-merged"));
    snapshot("tighten_allof", &tightened);
}

#[test]
fn allof_with_union_branch_skips() {
    let schema = json!({
        "type": "object",
        "properties": {
            "both": {
                "allOf": [
                    {"type": "object", "properties": {"a": {"type": "string"}}},
                    {"oneOf": [{"type": "string"}, {"type": "integer"}]},
                ],
            },
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("unsupported-allof"), "{error}");
}

#[test]
fn non_object_schema_skips() {
    for schema in [
        json!({"type": "array", "items": {"type": "string"}}),
        json!({"type": "string"}),
        Value::Null,
    ] {
        let error = tighten(schema).unwrap_err();
        assert!(error.starts_with("non-object-schema"), "{error}");
    }
}

#[test]
fn untyped_properties_become_any() {
    let schema = json!({
        "type": "object",
        "properties": {
            "anything": {},
            "picked": {"enum": ["a", "b"]},
        },
    });
    snapshot("tighten_any", &tighten(schema).unwrap());
}

#[test]
fn open_objects_warn_but_keep_properties() {
    let schema = json!({
        "type": "object",
        "properties": {
            "item": {
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "additionalProperties": true,
                "patternProperties": {"^x-": {"type": "string"}},
            },
        },
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("additional-properties-dropped"));
    assert!(tightened.warnings.contains("pattern-properties-dropped"));
}

#[test]
fn effect_labels_follow_annotations() {
    let plain = tool("plain", json!({"type": "object"}));
    let tightened = tighten_one(plain).unwrap();
    assert_eq!(
        tightened.effect,
        mcp2dekopon::tighten::Effect::ExternalWrite
    );
    assert_eq!(tightened.risk, mcp2dekopon::tighten::Risk::Medium);

    let mut read = tool("read", json!({"type": "object"}));
    read.read_only = Some(true);
    let tightened = tighten_one(read).unwrap();
    assert_eq!(tightened.effect, mcp2dekopon::tighten::Effect::ReadOnly);
    assert_eq!(tightened.risk, mcp2dekopon::tighten::Risk::Low);

    let mut destructive = tool("wipe", json!({"type": "object"}));
    destructive.destructive = Some(true);
    let tightened = tighten_one(destructive).unwrap();
    assert_eq!(
        tightened.effect,
        mcp2dekopon::tighten::Effect::ExternalWrite
    );
    assert_eq!(tightened.risk, mcp2dekopon::tighten::Risk::High);
}

#[test]
fn capability_names_dedupe_within_a_provider() {
    let mut names = HashSet::new();
    let mut types = HashSet::new();
    let first = tighten_tool(
        &tool("Foo Bar", json!({"type": "object"})),
        &mut names,
        200,
        &mut types,
    )
    .unwrap();
    let second = tighten_tool(
        &tool("foo-bar", json!({"type": "object"})),
        &mut names,
        200,
        &mut types,
    )
    .unwrap();
    assert_eq!(first.cap_name, "foo-bar");
    assert_eq!(second.cap_name, "foo-bar-2");
}

#[test]
fn keyword_fields_use_raw_idents() {
    let schema = json!({
        "type": "object",
        "properties": {
            "type": {"type": "string"},
            "self": {"type": "string"},
        },
    });
    let tightened = tighten_named("probe", schema).unwrap();
    let fields: Vec<&str> = tightened
        .fields
        .iter()
        .map(|f| f.rust_name.as_str())
        .collect();
    assert!(fields.contains(&"r#type"), "{fields:?}");
    assert!(fields.contains(&"self_"), "{fields:?}");
}
