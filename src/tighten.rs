//! Stage 2: tighten each MCP input schema until the provider SDK accepts it.
//!
//! The SDK refuses any `$ref` and any object without
//! `additionalProperties: false`. Rules, applied in order:
//!
//! - inline local `$ref`/`$defs`; a recursive reference skips the tool;
//! - every object becomes a closed struct; a free map (only
//!   `additionalProperties`, no `properties`) becomes a JSON string argument
//!   the generated code parses;
//! - `oneOf`/`anyOf` of objects becomes a Rust enum when every branch is
//!   closed, else the tool is skipped;
//! - nullable `anyOf`/`oneOf` (`[T, null]`) and `type: ["T", "null"]`
//!   collapse to `Option<T>` (the SDK-validated reading of the MCP nullable
//!   idiom).

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::fetch::McpTool;
use crate::names;

/// Why a tool was skipped. Every skip lands in the report; nothing is silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skip {
    pub reason: &'static str,
    pub detail: String,
}

impl Skip {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// A tool ready for codegen.
#[derive(Clone, Debug, Serialize)]
pub struct TightenedTool {
    /// The MCP tool name, sent verbatim in `tools/call`.
    pub mcp_name: String,
    /// The sanitized capability `NAME`.
    pub cap_name: String,
    pub description: String,
    pub fields: Vec<Field>,
    /// Set when the whole input is a free map: the input carries one JSON
    /// string that becomes the entire arguments object.
    pub top_level_map: bool,
    pub effect: Effect,
    pub risk: Risk,
    /// Structural transforms applied (`free-map-as-string`, `allof-merged`).
    pub flags: BTreeSet<String>,
    /// Fidelity notes for the operator.
    pub warnings: BTreeSet<String>,
}

/// Effect label for the manifest. A wrong `ReadOnly` would place the
/// capability in the provider's `read-only` Cedar group, so the default is
/// `ExternalWrite` and the report lists every label for review.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Effect {
    ReadOnly,
    ExternalWrite,
}

impl Effect {
    /// Renders the `EffectKind` path for generated code.
    pub fn token(self) -> &'static str {
        match self {
            Self::ReadOnly => "EffectKind::ReadOnly",
            Self::ExternalWrite => "EffectKind::ExternalWrite",
        }
    }

    /// Renders the wire string for the report.
    pub fn wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ExternalWrite => "external-write",
        }
    }
}

/// Risk label for the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    /// Renders the `RiskLevel` path for generated code.
    pub fn token(self) -> &'static str {
        match self {
            Self::Low => "RiskLevel::Low",
            Self::Medium => "RiskLevel::Medium",
            Self::High => "RiskLevel::High",
        }
    }

    /// Renders the display string for the report.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

/// One input property.
#[derive(Clone, Debug, Serialize)]
pub struct Field {
    /// The MCP property name, preserved with `#[serde(rename)]`.
    pub mcp_name: String,
    /// The `snake_case` Rust field (and kebab-case flag source).
    pub rust_name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    pub required: bool,
    pub description: String,
}

/// A lowered property type. Every variant renders a closed schema.
#[derive(Clone, Debug, Serialize)]
pub enum FieldType {
    String,
    Integer,
    Number,
    Boolean,
    /// A free map carried as JSON text and parsed by generated code.
    JsonString,
    /// Untyped (`{}`): `serde_json::Value`, schema `true`.
    Any,
    Array {
        item: Box<FieldType>,
    },
    /// A nullable array item: `Option<T>` inside the `Vec`.
    Nullable {
        inner: Box<FieldType>,
    },
    Object(ObjectShape),
    Enum(EnumShape),
}

/// A closed object: one generated struct.
#[derive(Clone, Debug, Serialize)]
pub struct ObjectShape {
    pub type_name: String,
    pub fields: Vec<Field>,
}

/// A `oneOf`/`anyOf` of objects: one untagged enum.
#[derive(Clone, Debug, Serialize)]
pub struct EnumShape {
    pub type_name: String,
    pub variants: Vec<EnumVariant>,
}

/// One enum branch.
#[derive(Clone, Debug, Serialize)]
pub struct EnumVariant {
    pub name: String,
    pub kind: VariantKind,
}

/// What an enum branch holds: a closed struct or a scalar shape.
#[derive(Clone, Debug, Serialize)]
pub enum VariantKind {
    Struct(ObjectShape),
    Scalar(FieldType),
}

impl FieldType {
    /// Whether the value travels through a JSON-text flag and `propose` parses.
    pub fn is_json_flag(&self) -> bool {
        matches!(
            self,
            Self::Array { .. }
                | Self::Nullable { .. }
                | Self::Object(_)
                | Self::Enum(_)
                | Self::Any
        )
    }
}

struct Context<'a> {
    root: &'a Value,
    defs: HashMap<String, &'a Value>,
    visiting: Vec<String>,
    used_types: HashSet<String>,
    tool_path: String,
    flags: BTreeSet<String>,
    warnings: BTreeSet<String>,
}

/// Tightens one tool. `used_names` dedupes capability names within a provider,
/// `max_name_len` keeps `provider.name` inside the identifier limit, and
/// `used_types` (shared across the provider's tools) keeps generated Rust
/// type names unique.
pub fn tighten_tool(
    tool: &McpTool,
    used_names: &mut HashSet<String>,
    max_name_len: usize,
    used_types: &mut HashSet<String>,
) -> Result<TightenedTool, Skip> {
    let cap_name = names::sanitize_tool_name(&tool.name, used_names, max_name_len)
        .map_err(|detail| Skip::new("invalid-name", format!("tool {:?}: {detail}", tool.name)))?;
    let (effect, risk) = classify(tool);
    let mut ctx = Context {
        root: &tool.input_schema,
        defs: HashMap::new(),
        visiting: Vec::new(),
        used_types: std::mem::take(used_types),
        tool_path: cap_name.clone(),
        flags: BTreeSet::new(),
        warnings: BTreeSet::new(),
    };
    ctx.defs = collect_defs(&tool.input_schema);
    let result = tighten_inner(tool, &cap_name, effect, risk, &mut ctx);
    *used_types = std::mem::take(&mut ctx.used_types);
    result
}

fn tighten_inner<'a>(
    tool: &'a McpTool,
    cap_name: &str,
    effect: Effect,
    risk: Risk,
    ctx: &mut Context<'a>,
) -> Result<TightenedTool, Skip> {
    let schema = resolve_root(&tool.input_schema, ctx)?;
    let Some(object) = schema.as_object() else {
        return Err(Skip::new(
            "non-object-schema",
            format!("tool {:?}: inputSchema is not an object", tool.name),
        ));
    };
    if let Some(kind) = object.get("type") {
        let nullable_object = kind == &Value::from("object")
            || kind == &Value::from(vec![Value::from("object"), Value::from("null")])
            || kind == &Value::from(vec![Value::from("null"), Value::from("object")]);
        if !nullable_object {
            return Err(Skip::new(
                "non-object-schema",
                format!("tool {:?}: inputSchema type must be object", tool.name),
            ));
        }
    }
    if object.contains_key("oneOf")
        || object.contains_key("anyOf")
        || object.contains_key("allOf")
        || object.contains_key("not")
    {
        return Err(Skip::new(
            "non-object-schema",
            format!("tool {:?}: inputSchema must be a plain object", tool.name),
        ));
    }
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if object.contains_key("patternProperties") {
        ctx.warnings
            .insert("pattern-properties-dropped".to_string());
    }
    let additional = object.get("additionalProperties");
    if additional.is_some_and(|a| a != &Value::Bool(false)) && !properties.is_empty() {
        ctx.warnings
            .insert("additional-properties-dropped".to_string());
    }
    // A top-level free map becomes one JSON string input.
    if properties.is_empty() && additional.is_some_and(|a| a != &Value::Bool(false)) {
        ctx.flags.insert("free-map-as-string".to_string());
        let field = Field {
            mcp_name: "payload".to_string(),
            rust_name: "payload".to_string(),
            field_type: FieldType::JsonString,
            required: true,
            description: "The arguments object as JSON text.".to_string(),
        };
        return Ok(TightenedTool {
            mcp_name: tool.name.clone(),
            cap_name: cap_name.to_string(),
            description: tool
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool {}", tool.name)),
            fields: vec![field],
            top_level_map: true,
            effect,
            risk,
            flags: std::mem::take(&mut ctx.flags),
            warnings: std::mem::take(&mut ctx.warnings),
        });
    }

    let required = required_set(object);
    let mut used_fields = HashSet::new();
    let mut fields = Vec::new();
    let mut names_sorted: Vec<(&String, &Value)> = properties.iter().collect();
    names_sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (mcp_name, schema) in names_sorted {
        let (field_type, nullable) =
            convert(schema, ctx, vec![mcp_name.clone()]).map_err(|skip| {
                if skip.reason == "recursive-schema" {
                    skip
                } else {
                    Skip::new(
                        skip.reason,
                        format!(
                            "tool {:?} property {mcp_name:?}: {}",
                            tool.name, skip.detail
                        ),
                    )
                }
            })?;
        let rust_name = names::rust_field_name(mcp_name, &mut used_fields);
        let description = schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        fields.push(Field {
            mcp_name: mcp_name.clone(),
            rust_name,
            field_type,
            required: required.contains(mcp_name.as_str()) && !nullable,
            description,
        });
    }
    Ok(TightenedTool {
        mcp_name: tool.name.clone(),
        cap_name: cap_name.to_string(),
        description: tool
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool {}", tool.name)),
        fields,
        top_level_map: false,
        effect,
        risk,
        flags: std::mem::take(&mut ctx.flags),
        warnings: std::mem::take(&mut ctx.warnings),
    })
}

fn classify(tool: &McpTool) -> (Effect, Risk) {
    if tool.destructive == Some(true) {
        (Effect::ExternalWrite, Risk::High)
    } else if tool.read_only == Some(true) {
        (Effect::ReadOnly, Risk::Low)
    } else {
        (Effect::ExternalWrite, Risk::Medium)
    }
}

fn required_set(object: &serde_json::Map<String, Value>) -> HashSet<&str> {
    object
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default()
}

fn resolve_root<'a>(schema: &'a Value, ctx: &mut Context<'a>) -> Result<&'a Value, Skip> {
    match schema.get("$ref").and_then(Value::as_str) {
        None => Ok(schema),
        Some(target) => resolve_chain(target, ctx),
    }
}

fn collect_defs(schema: &Value) -> HashMap<String, &Value> {
    let mut defs = HashMap::new();
    let Some(object) = schema.as_object() else {
        return defs;
    };
    for key in ["$defs", "definitions"] {
        if let Some(group) = object.get(key).and_then(Value::as_object) {
            for (name, value) in group {
                defs.insert(format!("#{key}/{name}"), value);
            }
        }
    }
    if let Some(schemas) = object
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object)
    {
        for (name, value) in schemas {
            defs.insert(format!("#/components/schemas/{name}"), value);
        }
    }
    defs
}

/// Looks up one local `$ref` target.
fn lookup_ref<'a>(target: &str, ctx: &Context<'a>) -> Result<&'a Value, Skip> {
    if !target.starts_with('#') {
        return Err(Skip::new(
            "external-ref",
            format!("reference {target:?} is not local and cannot be inlined"),
        ));
    }
    if let Some(hit) = ctx.defs.get(target) {
        return Ok(*hit);
    }
    resolve_pointer(ctx.root, target).ok_or_else(|| {
        Skip::new(
            "unresolved-ref",
            format!("reference {target:?} resolves to nothing"),
        )
    })
}

/// Follows a `$ref` chain to its object. Pure: cycle checks cover both the
/// chain itself and the refs whose expansion is already on the stack.
fn resolve_chain<'a>(target: &str, ctx: &Context<'a>) -> Result<&'a Value, Skip> {
    let mut seen = vec![target.to_string()];
    let mut current = lookup_ref(target, ctx)?;
    while let Some(next) = current.get("$ref").and_then(Value::as_str) {
        if !next.starts_with('#') {
            return Err(Skip::new(
                "external-ref",
                format!("reference {next:?} is not local and cannot be inlined"),
            ));
        }
        if ctx.visiting.iter().any(|v| v == next) || seen.iter().any(|v| v == next) {
            return Err(Skip::new(
                "recursive-schema",
                format!("reference {next:?} is recursive"),
            ));
        }
        seen.push(next.to_string());
        current = lookup_ref(next, ctx)?;
    }
    Ok(current)
}

fn resolve_pointer<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer == "#" || pointer == "#/" {
        return Some(root);
    }
    let path = pointer.strip_prefix("#/")?;
    let mut current = root;
    for raw in path.split('/') {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        match current {
            Value::Object(map) => current = map.get(&segment)?,
            Value::Array(items) => current = items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        }
    }
    Some(current)
}

/// Lowers one property schema. Returns the type and whether a null branch
/// forces `Option`.
fn convert<'a, 's>(
    schema: &'s Value,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
        return convert_ref(target, ctx, path);
    }
    convert_inner(schema, ctx, path)
}

/// Converts through a `$ref`, holding the target on the stack for the whole
/// expansion so a cycle back to it is a skip, not unbounded recursion.
fn convert_ref<'a>(
    target: &str,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip> {
    if ctx.visiting.iter().any(|v| v == target) {
        return Err(Skip::new(
            "recursive-schema",
            format!("reference {target:?} is recursive"),
        ));
    }
    let resolved = resolve_chain(target, ctx)?;
    ctx.visiting.push(target.to_string());
    let result = convert_inner(resolved, ctx, path);
    ctx.visiting.pop();
    result
}

fn convert_inner<'a, 's>(
    schema: &'s Value,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    let Some(object) = schema.as_object() else {
        return Err(Skip::new(
            "unsupported-schema",
            "a property schema must be an object".to_string(),
        ));
    };

    if let Some(combined) = object.get("oneOf").or_else(|| object.get("anyOf")) {
        return convert_combined(combined, ctx, path);
    }
    if let Some(all) = object.get("allOf") {
        return convert_all_of(all, ctx, path);
    }

    match object.get("type") {
        Some(Value::String(kind)) => convert_typed(kind.as_str(), object, ctx, path),
        Some(Value::Array(kinds)) => convert_type_array(kinds, object, ctx, path),
        Some(_) => Err(Skip::new(
            "unsupported-schema",
            "a property type must be a string or an array".to_string(),
        )),
        None => convert_untyped(object, ctx, path),
    }
}

fn convert_typed<'a, 's>(
    kind: &str,
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    match kind {
        "string" => {
            if object.contains_key("enum") || object.contains_key("const") {
                ctx.warnings.insert("enum-not-constrained".to_string());
            }
            Ok((FieldType::String, false))
        }
        "integer" => Ok((FieldType::Integer, false)),
        "number" => Ok((FieldType::Number, false)),
        "boolean" => Ok((FieldType::Boolean, false)),
        "null" => {
            ctx.warnings.insert("null-type-loosened".to_string());
            Ok((FieldType::Any, true))
        }
        "array" => Ok((convert_array(object, ctx, path)?, false)),
        "object" => Ok((convert_object(object, ctx, path)?, false)),
        _ => {
            ctx.warnings.insert("unknown-type-loosened".to_string());
            Ok((FieldType::Any, false))
        }
    }
}

fn convert_type_array<'a, 's>(
    kinds: &'s [Value],
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    let mut real: Vec<&str> = Vec::new();
    let mut nullable = false;
    for kind in kinds {
        match kind.as_str() {
            Some("null") => nullable = true,
            Some(kind) => real.push(kind),
            None => {
                return Err(Skip::new(
                    "unsupported-schema",
                    "a property type array must hold strings".to_string(),
                ));
            }
        }
    }
    if real.len() == 1 {
        let (field_type, _) = convert_typed(real[0], object, ctx, path)?;
        return Ok((field_type, nullable));
    }
    Err(Skip::new(
        "unsupported-union",
        "a type union must hold one type plus null".to_string(),
    ))
}

fn convert_untyped<'a, 's>(
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    if object.contains_key("properties")
        || object.contains_key("additionalProperties")
        || object.contains_key("patternProperties")
    {
        return Ok((convert_object(object, ctx, path)?, false));
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        if let Some(first) = values.first() {
            if values.iter().all(|v| same_json_kind(v, first)) {
                if first.is_string() {
                    ctx.warnings.insert("enum-not-constrained".to_string());
                }
                return Ok((scalar_for(first), false));
            }
        }
        return Ok((FieldType::Any, false));
    }
    if let Some(value) = object.get("const") {
        if !value.is_object() && !value.is_array() {
            return Ok((scalar_for(value), false));
        }
        ctx.warnings.insert("enum-not-constrained".to_string());
        return Ok((FieldType::Any, false));
    }
    Ok((FieldType::Any, false))
}

fn same_json_kind(left: &Value, right: &Value) -> bool {
    matches!(
        (left, right),
        (Value::String(_), Value::String(_))
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::Null, Value::Null)
    )
}

fn scalar_for(value: &Value) -> FieldType {
    if value.is_string() {
        FieldType::String
    } else if value.is_i64() || value.is_u64() {
        FieldType::Integer
    } else if value.is_number() {
        FieldType::Number
    } else if value.is_boolean() {
        FieldType::Boolean
    } else {
        FieldType::Any
    }
}

fn convert_combined<'a, 's>(
    combined: &'s Value,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    let Some(branches) = combined.as_array() else {
        return Err(Skip::new(
            "unsupported-union",
            "oneOf/anyOf must be an array".to_string(),
        ));
    };
    if branches.is_empty() {
        return Err(Skip::new(
            "unsupported-union",
            "oneOf/anyOf must not be empty".to_string(),
        ));
    }
    // Nullable collapse: [T, null] becomes Option<T>.
    let mut real: Vec<&Value> = Vec::new();
    for branch in branches {
        let resolved = if branch.get("$ref").is_some() {
            let target = branch.get("$ref").and_then(Value::as_str).unwrap_or("#");
            resolve_chain(target, ctx)?
        } else {
            branch
        };
        if resolved.get("type") == Some(&Value::from("null")) {
            continue;
        }
        real.push(resolved);
    }
    let nullable = real.len() != branches.len();
    if real.len() == 1 && nullable {
        let (field_type, _) = convert(real[0], ctx, path)?;
        return Ok((field_type, true));
    }
    if real.len() == 1 && branches.len() == 1 {
        return convert(real[0], ctx, path);
    }
    if real.is_empty() {
        return Ok((FieldType::Any, true));
    }
    // Enum of objects: every branch must close.
    let all_objects = real
        .iter()
        .map(|branch| branch.as_object().filter(|object| is_plain_object(object)))
        .collect::<Option<Vec<_>>>();
    if let Some(objects) = all_objects {
        let mut variants = Vec::new();
        for (index, (branch, object)) in real.iter().zip(objects.iter()).enumerate() {
            let mut branch_path = path.clone();
            branch_path.push(format!("branch{index}"));
            let shape = shape_for_object(object, ctx, branch_path)?;
            let Some(shape) = shape else {
                return Err(Skip::new(
                    "unsupported-union",
                    "oneOf/anyOf branches must all be closed objects".to_string(),
                ));
            };
            let title = branch.get("title").and_then(Value::as_str).unwrap_or("");
            let name = if title.is_empty() {
                format!("Branch{index}")
            } else {
                names::rust_type_name(&[title], &mut HashSet::new())
            };
            variants.push(EnumVariant {
                name,
                kind: VariantKind::Struct(shape),
            });
        }
        return Ok(finish_enum(variants, nullable, ctx, &path));
    }
    // Scalar-or-array union (string, list of strings, ...): an untagged enum
    // over scalar shapes. Anything else is genuinely unlowerable.
    scalar_union(&real)
        .map(|variants| finish_enum(variants, nullable, ctx, &path))
        .map_err(|detail| Skip::new("unsupported-union", detail))
}

/// Dedupes variant names, assigns the enum type name, and wraps the result.
fn finish_enum(
    mut variants: Vec<EnumVariant>,
    nullable: bool,
    ctx: &mut Context<'_>,
    path: &[String],
) -> (FieldType, bool) {
    let mut seen = HashSet::new();
    for variant in variants.iter_mut() {
        let mut candidate = variant.name.clone();
        let mut counter = 2_u32;
        while seen.contains(&candidate) {
            candidate = format!("{}{counter}", variant.name);
            counter += 1;
        }
        variant.name = candidate.clone();
        seen.insert(candidate);
    }
    let joined = path.join("_");
    let type_name = names::rust_type_name(
        &[ctx.tool_path.as_str(), joined.as_str(), "value"],
        &mut ctx.used_types,
    );
    (
        FieldType::Enum(EnumShape {
            type_name,
            variants,
        }),
        nullable,
    )
}

/// Lowers a scalar-or-array union (string, list of strings, ...) to untagged
/// enum variants. `f64` shapes sort after integer ones so untagged
/// deserialization tries the exact match first.
fn scalar_union(branches: &[&Value]) -> Result<Vec<EnumVariant>, String> {
    let mut variants = Vec::new();
    let mut seen_shapes = HashSet::new();
    for branch in branches {
        let Some(object) = branch.as_object() else {
            return Err("oneOf/anyOf branches must all be objects or scalars".to_string());
        };
        let shape = scalar_shape(object)
            .ok_or_else(|| "oneOf/anyOf branches must all be objects or scalars".to_string())?;
        if !seen_shapes.insert(shape_key(&shape)) {
            continue;
        }
        variants.push(EnumVariant {
            name: variant_name(&shape),
            kind: VariantKind::Scalar(shape),
        });
    }
    if variants.len() < 2 {
        return Err("oneOf/anyOf branches must all be objects or scalars".to_string());
    }
    variants.sort_by_key(|variant| match &variant.kind {
        VariantKind::Scalar(shape) => has_float(shape),
        VariantKind::Struct(_) => false,
    });
    Ok(variants)
}

/// Reads one scalar branch: a scalar, or a one-level array of scalars.
fn scalar_shape(object: &serde_json::Map<String, Value>) -> Option<FieldType> {
    if object.contains_key("oneOf")
        || object.contains_key("anyOf")
        || object.contains_key("allOf")
        || object.contains_key("$ref")
        || object.contains_key("properties")
    {
        return None;
    }
    match object.get("type").and_then(Value::as_str) {
        Some("string") => Some(FieldType::String),
        Some("integer") => Some(FieldType::Integer),
        Some("number") => Some(FieldType::Number),
        Some("boolean") => Some(FieldType::Boolean),
        Some("array") => match object.get("items") {
            None => Some(FieldType::Array {
                item: Box::new(FieldType::Any),
            }),
            Some(items) => {
                let item = items.as_object()?;
                if item.contains_key("oneOf")
                    || item.contains_key("anyOf")
                    || item.contains_key("allOf")
                    || item.contains_key("$ref")
                    || item.contains_key("properties")
                {
                    return None;
                }
                let scalar = match item.get("type").and_then(Value::as_str) {
                    Some("string") => FieldType::String,
                    Some("integer") => FieldType::Integer,
                    Some("number") => FieldType::Number,
                    Some("boolean") => FieldType::Boolean,
                    _ => return None,
                };
                Some(FieldType::Array {
                    item: Box::new(scalar),
                })
            }
        },
        _ => None,
    }
}

fn shape_key(shape: &FieldType) -> String {
    match shape {
        FieldType::String => "s".to_string(),
        FieldType::Integer => "i".to_string(),
        FieldType::Number => "n".to_string(),
        FieldType::Boolean => "b".to_string(),
        FieldType::Array { item } => format!("a{}", shape_key(item)),
        _ => "x".to_string(),
    }
}

fn variant_name(shape: &FieldType) -> String {
    let base = match shape {
        FieldType::String => "single",
        FieldType::Integer => "integer",
        FieldType::Number => "number",
        FieldType::Boolean => "flag",
        FieldType::Array { item } => match item.as_ref() {
            FieldType::String => "text_list",
            FieldType::Integer => "integer_list",
            FieldType::Number => "number_list",
            FieldType::Boolean => "flag_list",
            _ => "list",
        },
        _ => "value",
    };
    let mut name = String::new();
    for word in base.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
            name.push_str(chars.as_str());
        }
    }
    name
}

fn has_float(shape: &FieldType) -> bool {
    match shape {
        FieldType::Number => true,
        FieldType::Array { item } => has_float(item),
        _ => false,
    }
}

fn is_plain_object(object: &serde_json::Map<String, Value>) -> bool {
    if object.contains_key("$ref")
        || object.contains_key("oneOf")
        || object.contains_key("anyOf")
        || object.contains_key("allOf")
    {
        return false;
    }
    match object.get("type") {
        None => true,
        Some(Value::String(kind)) => kind == "object",
        _ => false,
    }
}

fn convert_all_of<'a, 's>(
    all: &'s Value,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<(FieldType, bool), Skip>
where
    'a: 's,
{
    let Some(branches) = all.as_array() else {
        return Err(Skip::new(
            "unsupported-allof",
            "allOf must be an array".to_string(),
        ));
    };
    if branches.is_empty() {
        return Err(Skip::new(
            "unsupported-allof",
            "allOf must not be empty".to_string(),
        ));
    }
    let mut merged: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut properties = serde_json::Map::new();
    let mut required: Vec<Value> = Vec::new();
    for branch in branches {
        let resolved = if branch.get("$ref").is_some() {
            let target = branch.get("$ref").and_then(Value::as_str).unwrap_or("#");
            resolve_chain(target, ctx)?
        } else {
            branch
        };
        let Some(object) = resolved.as_object() else {
            return Err(Skip::new(
                "unsupported-allof",
                "allOf branches must all be objects".to_string(),
            ));
        };
        if !is_plain_object(object) {
            return Err(Skip::new(
                "unsupported-allof",
                "allOf branches must all be plain objects".to_string(),
            ));
        }
        if let Some(props) = object.get("properties").and_then(Value::as_object) {
            for (key, value) in props {
                if properties.contains_key(key) {
                    ctx.warnings.insert("allof-prop-conflict".to_string());
                } else {
                    properties.insert(key.clone(), value.clone());
                }
            }
        }
        if let Some(items) = object.get("required").and_then(Value::as_array) {
            for item in items {
                if !required.contains(item) {
                    required.push(item.clone());
                }
            }
        }
        if object
            .get("additionalProperties")
            .is_some_and(|a| a != &Value::Bool(false))
        {
            ctx.warnings
                .insert("additional-properties-dropped".to_string());
        }
    }
    merged.insert("type".to_string(), Value::from("object"));
    merged.insert("properties".to_string(), Value::Object(properties));
    merged.insert("required".to_string(), Value::Array(required));
    ctx.flags.insert("allof-merged".to_string());
    Ok((convert_object(&merged, ctx, path)?, false))
}

fn convert_array<'a, 's>(
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<FieldType, Skip>
where
    'a: 's,
{
    match object.get("items") {
        None => Ok(FieldType::Array {
            item: Box::new(FieldType::Any),
        }),
        Some(items) => {
            let mut item_path = path;
            item_path.push("item".to_string());
            let (item, nullable) = convert(items, ctx, item_path)?;
            let item = if nullable {
                FieldType::Nullable {
                    inner: Box::new(item),
                }
            } else {
                item
            };
            Ok(FieldType::Array {
                item: Box::new(item),
            })
        }
    }
}

fn convert_object<'a, 's>(
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<FieldType, Skip>
where
    'a: 's,
{
    match shape_for_object(object, ctx, path)? {
        Some(shape) => Ok(FieldType::Object(shape)),
        None => {
            ctx.flags.insert("free-map-as-string".to_string());
            Ok(FieldType::JsonString)
        }
    }
}

/// Builds the struct shape for an object, or `None` for a free map.
fn shape_for_object<'a, 's>(
    object: &'s serde_json::Map<String, Value>,
    ctx: &mut Context<'a>,
    path: Vec<String>,
) -> Result<Option<ObjectShape>, Skip>
where
    'a: 's,
{
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if object.contains_key("patternProperties") {
        ctx.warnings
            .insert("pattern-properties-dropped".to_string());
    }
    let additional = object.get("additionalProperties");
    if additional.is_some_and(|a| a != &Value::Bool(false)) {
        if properties.is_empty() {
            return Ok(None);
        }
        ctx.warnings
            .insert("additional-properties-dropped".to_string());
    }
    let required = required_set(object);
    let segments: Vec<&str> = std::iter::once(ctx.tool_path.as_str())
        .chain(path.iter().map(String::as_str))
        .collect();
    let type_name = names::rust_type_name(&segments, &mut ctx.used_types);
    let mut used_fields = HashSet::new();
    let mut fields = Vec::new();
    let mut names_sorted: Vec<(&String, &Value)> = properties.iter().collect();
    names_sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (mcp_name, schema) in names_sorted {
        let mut child_path = path.clone();
        child_path.push(mcp_name.clone());
        let (field_type, nullable) = convert(schema, ctx, child_path)?;
        fields.push(Field {
            rust_name: names::rust_field_name(mcp_name, &mut used_fields),
            mcp_name: mcp_name.clone(),
            field_type,
            required: required.contains(mcp_name.as_str()) && !nullable,
            description: schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(Some(ObjectShape { type_name, fields }))
}
