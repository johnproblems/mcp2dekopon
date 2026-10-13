//! Name sanitization: MCP names become capability names, Rust idents and flags.
//!
//! Identifier rules mirror `dekopon-core`: lowercase ASCII letters and digits
//! at the edges, `.`, `-`, `_` separators inside, never adjacent, at most 253
//! bytes.

use std::collections::HashSet;

pub const MAX_IDENTIFIER_LEN: usize = 253;

/// Validates a `--id` provider id without changing it; ids are identity.
pub fn validate_provider_id(raw: &str) -> Result<String, String> {
    validate_identifier(raw, "provider id")?;
    Ok(raw.to_string())
}

fn validate_identifier(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{kind} must not be empty"));
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err(format!(
            "{kind} is {} bytes; the maximum is {MAX_IDENTIFIER_LEN}",
            value.len()
        ));
    }
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = value.chars();
    let first = chars.next().unwrap_or('\0');
    if !edge(first) {
        return Err(format!(
            "{kind} must start with a lowercase ASCII letter or digit, found {first:?}"
        ));
    }
    let mut previous_was_separator = false;
    for (index, character) in value.char_indices() {
        let separator = matches!(character, '.' | '-' | '_');
        if !edge(character) && !separator {
            return Err(format!(
                "{kind} contains invalid character {character:?} at byte {index}; \
                 use lowercase ASCII letters, digits, '.', '-', or '_'"
            ));
        }
        if separator && previous_was_separator {
            return Err(format!(
                "{kind} contains adjacent separators at byte {index}"
            ));
        }
        previous_was_separator = separator;
    }
    let last = value.chars().next_back().unwrap_or('\0');
    if !edge(last) {
        return Err(format!(
            "{kind} must end with a lowercase ASCII letter or digit, found {last:?}"
        ));
    }
    Ok(())
}

/// Sanitizes an MCP tool name into a capability `NAME`, deduplicated via `used`.
/// `max_len` bounds the name so `provider.name` still fits the identifier limit.
pub fn sanitize_tool_name(
    raw: &str,
    used: &mut HashSet<String>,
    max_len: usize,
) -> Result<String, String> {
    let mut base = String::with_capacity(raw.len());
    let mut previous_was_separator = true;
    for character in raw.chars().map(|c| c.to_ascii_lowercase()) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            base.push(character);
            previous_was_separator = false;
        } else if matches!(character, '.' | '-' | '_') {
            if !previous_was_separator {
                base.push(character);
                previous_was_separator = true;
            }
        } else if !previous_was_separator {
            base.push('-');
            previous_was_separator = true;
        }
    }
    while base.ends_with(['.', '-', '_']) {
        base.pop();
    }
    if base.is_empty() {
        return Err(format!("tool name {raw:?} has no usable characters"));
    }
    let mut candidate = truncate_ident(&base, max_len);
    if candidate.is_empty() {
        return Err(format!("tool name {raw:?} does not fit the length limit"));
    }
    let mut counter = 2_u32;
    while used.contains(&candidate) {
        let suffix = format!("-{counter}");
        let room = max_len.saturating_sub(suffix.len());
        candidate = format!("{}{suffix}", truncate_ident(&base, room));
        counter += 1;
        if counter > 10000 {
            return Err(format!("tool name {raw:?} cannot be deduplicated"));
        }
    }
    used.insert(candidate.clone());
    Ok(candidate)
}

fn truncate_ident(value: &str, max_len: usize) -> String {
    let mut out: String = value.chars().take(max_len).collect();
    while out.ends_with(['.', '-', '_']) {
        out.pop();
    }
    out
}

/// Converts an MCP property name to a `snake_case` Rust field name, deduplicated
/// via `used`. The MCP spelling is preserved separately with `#[serde(rename)]`.
pub fn rust_field_name(mcp: &str, used: &mut HashSet<String>) -> String {
    // Split on non-alphanumerics and on camel humps (dryRun -> dry_run).
    let chars: Vec<char> = mcp.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, &character) in chars.iter().enumerate() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let previous = index.checked_sub(1).and_then(|i| chars.get(i));
        let next = chars.get(index + 1);
        let hump = character.is_ascii_uppercase()
            && !current.is_empty()
            && (previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || next.is_some_and(|n| n.is_ascii_lowercase()));
        if hump {
            words.push(std::mem::take(&mut current));
        }
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut base = if words.is_empty() {
        "field".to_string()
    } else {
        words.join("_")
    };
    if base.starts_with(|c: char| c.is_ascii_digit()) {
        base = format!("f_{base}");
    }
    if base == "_" {
        base = "field".to_string();
    }
    let mut candidate = base.clone();
    let mut counter = 2_u32;
    while used.contains(&candidate) {
        candidate = format!("{base}_{counter}");
        counter += 1;
    }
    used.insert(candidate.clone());
    if candidate == "self" || candidate == "Self" {
        return format!("{candidate}_");
    }
    if is_rust_keyword(&candidate) {
        return format!("r#{candidate}");
    }
    candidate
}

/// Converts identifier segments to a `PascalCase` Rust type name, deduplicated
/// via `used`.
pub fn rust_type_name(segments: &[&str], used: &mut HashSet<String>) -> String {
    let mut base: String = segments
        .iter()
        .flat_map(|s| s.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let mut out = String::new();
            if let Some(first) = chars.next() {
                out.push(first.to_ascii_uppercase());
            }
            out.push_str(&chars.as_str().to_ascii_lowercase());
            out
        })
        .collect();
    if base.is_empty() || base.starts_with(|c: char| c.is_ascii_digit()) {
        base = format!("T{base}");
    }
    let mut candidate = base.clone();
    let mut counter = 2_u32;
    while used.contains(&candidate) {
        candidate = format!("{base}{counter}");
        counter += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn is_rust_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "try"
            | "gen"
    )
}

/// Derives a crate name from a provider id (`-`/`.` become `_`, digit start
/// gets an `mcp_` prefix, Rust keywords get a `_provider` suffix).
pub fn crate_name(provider_id: &str) -> String {
    let mut name: String = provider_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name = format!("mcp_{name}");
    }
    if is_rust_keyword(&name) || name == "self" || name == "Self" || name == "_" {
        name = format!("{name}_provider");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_follow_identifier_rules() {
        assert!(validate_provider_id("deepwiki").is_ok());
        assert!(validate_provider_id("my-mcp_2.0").is_ok());
        assert!(validate_provider_id("Deepwiki").is_err());
        assert!(validate_provider_id("-wiki").is_err());
        assert!(validate_provider_id("wiki-").is_err());
        assert!(validate_provider_id("wi--ki").is_err());
        assert!(validate_provider_id("wi ki").is_err());
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id(&"a".repeat(254)).is_err());
    }

    #[test]
    fn tool_names_sanitize_and_dedupe() {
        let mut used = HashSet::new();
        assert_eq!(
            sanitize_tool_name("read_wiki_contents", &mut used, 200).unwrap(),
            "read_wiki_contents"
        );
        assert_eq!(
            sanitize_tool_name("readWikiContents", &mut used, 200).unwrap(),
            "readwikicontents"
        );
        assert_eq!(
            sanitize_tool_name("Read Wiki Contents!", &mut used, 200).unwrap(),
            "read-wiki-contents"
        );
        assert_eq!(
            sanitize_tool_name("read-wiki-contents", &mut used, 200).unwrap(),
            "read-wiki-contents-2"
        );
        assert!(sanitize_tool_name("!!!", &mut used, 200).is_err());
    }

    #[test]
    fn field_names_are_valid_idents() {
        let mut used = HashSet::new();
        assert_eq!(rust_field_name("dryRun", &mut used), "dry_run");
        assert_eq!(rust_field_name("dry_run", &mut used), "dry_run_2");
        assert_eq!(rust_field_name("type", &mut used), "r#type");
        assert_eq!(rust_field_name("self", &mut used), "self_");
        assert_eq!(rust_field_name("9lives", &mut used), "f_9lives");
        assert_eq!(rust_field_name("a.b-c d", &mut used), "a_b_c_d");
    }

    #[test]
    fn type_names_are_pascal_and_unique() {
        let mut used = HashSet::new();
        assert_eq!(
            rust_type_name(&["read_wiki", "filter"], &mut used),
            "ReadWikiFilter"
        );
        assert_eq!(
            rust_type_name(&["read_wiki", "filter"], &mut used),
            "ReadWikiFilter2"
        );
        assert_eq!(rust_type_name(&["9", "x"], &mut used), "T9X");
    }

    #[test]
    fn crate_names_are_safe() {
        assert_eq!(crate_name("deepwiki"), "deepwiki");
        assert_eq!(crate_name("my-mcp.v2"), "my_mcp_v2");
        assert_eq!(crate_name("2fast"), "mcp_2fast");
        assert_eq!(crate_name("type"), "type_provider");
        assert_eq!(crate_name("self"), "self_provider");
    }

    #[test]
    fn long_tool_names_truncate_to_fit() {
        let mut used = HashSet::new();
        let name = sanitize_tool_name(&"a".repeat(300), &mut used, 253).unwrap();
        assert_eq!(name.len(), 253);
        let dup = sanitize_tool_name(&"a".repeat(300), &mut used, 253).unwrap();
        assert!(dup.len() <= 253 && dup.ends_with("-2"), "{dup}");
        let tiny = sanitize_tool_name("tool-name", &mut HashSet::new(), 3).unwrap();
        assert_eq!(tiny, "too");
        // Separators exposed by the cut are trimmed, not left dangling.
        let cut = sanitize_tool_name("ab-cdef", &mut HashSet::new(), 3).unwrap();
        assert_eq!(cut, "ab");
    }
}
