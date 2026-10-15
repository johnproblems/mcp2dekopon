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
        input_schema: schema.clone(),
        read_only: None,
        destructive: None,
        raw: json!({"name": name, "inputSchema": schema}),
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
fn pathological_schemas_never_panic() {
    let horrors = vec![
        json!({"type": "object", "properties": {"x": {"type": 42}}}),
        json!({"type": "object", "properties": {"x": [1, 2, 3]}}),
        json!({"type": "object", "properties": Value::Null}),
        json!({"type": "object", "required": "query"}),
        json!({"type": "object", "properties": {"x": {"oneOf": "nope"}}}),
        json!({"type": "object", "properties": {"x": {"allOf": []}}}),
        json!({"type": "object", "properties": {"x": {"type": ["string", 7]}}}),
        json!({"type": "object", "properties": {"x": {"enum": []}}}),
        json!({"type": "object", "properties": {"": {"type": "string"}}}),
        json!({"type": "object", "properties": {"x": {"type": "array", "items": [1]}}}),
        json!({"type": "object", "properties": {"x": {"$ref": 7}}}),
        json!({"type": "object", "properties": {"x": {"anyOf": [{}, {}]}}}),
    ];
    for schema in horrors {
        let _ = tighten(schema);
    }
}

#[test]
fn deep_nesting_skips_instead_of_recursing() {
    let mut inner = json!({"type": "string"});
    for _ in 0..40 {
        inner = json!({
            "type": "object",
            "properties": {"nested": inner},
        });
    }
    let schema = json!({"type": "object", "properties": {"top": inner}});
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("schema-too-deep"), "{error}");
}

#[test]
fn pointer_refs_resolve() {
    let schema = json!({
        "type": "object",
        "properties": {
            "original": {
                "type": "object",
                "properties": {"x": {"type": "string"}},
                "required": ["x"],
            },
            "alias": {"$ref": "#/properties/original"},
        },
        "required": ["original", "alias"],
    });
    snapshot("tighten_pointer_refs", &tighten(schema).unwrap());
}

#[test]
fn combined_branches_resolve_through_refs() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "A": {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
            "B": {"type": "object", "properties": {"b": {"type": "integer"}}},
        },
        "properties": {
            "either": {"oneOf": [{"$ref": "#/$defs/A"}, {"$ref": "#/$defs/B"}]},
            "both": {"allOf": [{"$ref": "#/$defs/A"}, {"$ref": "#/$defs/B"}]},
        },
        "required": ["either", "both"],
    });
    snapshot("tighten_ref_combined", &tighten(schema).unwrap());
}

#[test]
fn empty_object_properties_lower() {
    let schema = json!({
        "type": "object",
        "properties": {
            "options": {"type": "object"},
            "anything": {},
        },
    });
    snapshot("tighten_empty_object", &tighten(schema).unwrap());
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

#[test]
fn boolean_schemas_lower_to_any() {
    let schema = json!({
        "type": "object",
        "properties": {
            "anything": true,
            "nothing": false,
        },
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("false-schema-loosened"));
    snapshot("tighten_boolean_schemas", &tightened);
}

#[test]
fn empty_type_array_loosens_instead_of_skipping() {
    let schema = json!({
        "type": "object",
        "properties": {
            "vague": {"type": []},
            "nil": {"type": ["null"]},
        },
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("empty-type-loosened"));
    snapshot("tighten_empty_type", &tightened);
}

#[test]
fn dropped_constraints_warn_loudly() {
    let schema = json!({
        "type": "object",
        "not": {"type": "null"},
        "properties": {
            "count": {"type": "integer", "if": {"minimum": 1}, "then": {"maximum": 9}},
            "pair": {"type": "array", "prefixItems": [{"type": "string"}], "items": {"type": "string"}},
        },
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("schema-constraint-dropped"));
    assert!(tightened.warnings.contains("tuple-validation-dropped"));
    snapshot("tighten_dropped_constraints", &tightened);
}

#[test]
fn mixed_and_empty_enums_warn() {
    let schema = json!({
        "type": "object",
        "properties": {
            "mixed": {"enum": ["a", 1]},
            "empty": {"enum": []},
        },
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.warnings.contains("enum-not-constrained"));
    snapshot("tighten_mixed_enum", &tightened);
}

#[test]
fn identical_scalar_union_collapses() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": {"oneOf": [{"type": "string"}, {"type": "string"}]},
        },
        "required": ["value"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(matches!(
        tightened.fields[0].field_type,
        mcp2dekopon::tighten::FieldType::String
    ));
    snapshot("tighten_scalar_collapse", &tightened);
}

#[test]
fn nullable_object_branches_join_unions() {
    let schema = json!({
        "type": "object",
        "properties": {
            "target": {
                "oneOf": [
                    {"type": ["object", "null"], "properties": {"repo": {"type": "string"}}, "required": ["repo"]},
                    {"type": "object", "properties": {"gist": {"type": "string"}}},
                ],
            },
        },
        "required": ["target"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(!tightened.fields[0].required);
    snapshot("tighten_nullable_union", &tightened);
}

#[test]
fn nested_allof_flattens() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "Base": {
                "allOf": [
                    {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                    {"type": "object", "properties": {"b": {"type": "integer"}}},
                ],
            },
        },
        "properties": {
            "merged": {"allOf": [{"$ref": "#/$defs/Base"}, {"type": ["object", "null"], "properties": {"c": {"type": "boolean"}}}]},
        },
        "required": ["merged"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(tightened.flags.contains("allof-merged"));
    snapshot("tighten_nested_allof", &tightened);
}

#[test]
fn recursive_allof_skips() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "A": {"allOf": [{"$ref": "#/$defs/B"}, {"type": "object", "properties": {"a": {"type": "string"}}}]},
            "B": {"allOf": [{"$ref": "#/$defs/A"}, {"type": "object", "properties": {"b": {"type": "string"}}}]},
        },
        "properties": {
            "merged": {"$ref": "#/$defs/A"},
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("recursive-schema"), "{error}");
}

#[test]
fn nested_combined_flattens_through_refs() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "Alias": {"oneOf": [{"type": "string"}, {"type": "integer"}]},
        },
        "properties": {
            "value": {"oneOf": [{"$ref": "#/$defs/Alias"}, {"type": "boolean"}]},
        },
        "required": ["value"],
    });
    snapshot("tighten_nested_combined", &tighten(schema).unwrap());
}

#[test]
fn recursive_combined_skips() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "Loop": {"oneOf": [{"$ref": "#/$defs/Loop"}, {"type": "string"}]},
        },
        "properties": {
            "value": {"$ref": "#/$defs/Loop"},
        },
    });
    let error = tighten(schema).unwrap_err();
    assert!(error.starts_with("recursive-schema"), "{error}");
}

#[test]
fn percent_encoded_refs_resolve() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "weird name": {"type": "string"},
        },
        "properties": {
            "value": {"$ref": "#/$defs/weird%20name"},
        },
        "required": ["value"],
    });
    let tightened = tighten(schema).unwrap();
    assert!(matches!(
        tightened.fields[0].field_type,
        mcp2dekopon::tighten::FieldType::String
    ));
    snapshot("tighten_percent_ref", &tightened);
}
