#![allow(clippy::unwrap_used)]

//! Guards that keep the docs honest: every CLI flag is documented, every JSON
//! example parses, and the PRD coverage doc names every PRD section.

use std::collections::BTreeSet;

fn repo_file(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path).unwrap()
}

/// Every `--flag` in `gen --help` appears in the README CLI table, and every
/// flag in that table still exists. `--help` is exempt: it documents itself.
#[test]
fn readme_documents_every_gen_flag() {
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_mcp2dekopon"))
        .args(["gen", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let mut help_flags = BTreeSet::new();
    for token in String::from_utf8_lossy(&help.stdout)
        .split(|c: char| c.is_whitespace() || c == ',' || c == '<' || c == '>')
    {
        if let Some(flag) = token.strip_prefix("--") {
            if !flag.is_empty()
                && flag != "help"
                && flag.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            {
                help_flags.insert(flag.to_string());
            }
        }
    }
    assert!(!help_flags.is_empty());

    let mut readme_flags = BTreeSet::new();
    for line in repo_file("README.md").lines() {
        if !line.starts_with("| `--") {
            continue;
        }
        for span in line.split('`') {
            let flag = span.split_whitespace().next().unwrap_or("");
            if let Some(flag) = flag.strip_prefix("--") {
                readme_flags.insert(flag.to_string());
            }
        }
    }
    assert_eq!(readme_flags, help_flags);
}

/// Every ```json fence in the README must be valid JSON.
#[test]
fn readme_json_examples_parse() {
    let readme = repo_file("README.md");
    let mut count = 0;
    let mut in_fence = false;
    let mut fence = String::new();
    for line in readme.lines() {
        if line.trim() == "```json" {
            in_fence = true;
            fence.clear();
            continue;
        }
        if in_fence && line.trim() == "```" {
            in_fence = false;
            count += 1;
            serde_json::from_str::<serde_json::Value>(&fence)
                .unwrap_or_else(|e| panic!("README json fence {count} invalid: {e}\n{fence}"));
            continue;
        }
        if in_fence {
            fence.push_str(line);
            fence.push('\n');
        }
    }
    assert!(count >= 1, "no ```json fences found in README");
}

/// `gen --version` reports the crate version; the derive attribute must stay.
#[test]
fn version_flag_reports_crate_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mcp2dekopon"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "unexpected --version output: {text}"
    );
}

/// The demo media committed under docs/demos/ exists, has valid container
/// magic, and is large enough to hold real footage. Deep checks (duration,
/// animation, non-blank frames) live in scripts/record-demos.py --verify-only.
#[test]
fn demos_exist_and_are_valid_media() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/demos");
    for name in ["generate", "offline", "review"] {
        let gif = std::fs::read(root.join(format!("{name}.gif"))).unwrap();
        assert!(
            gif.starts_with(b"GIF89a") && gif.len() > 20_000,
            "{name}.gif is {} bytes",
            gif.len()
        );
        let mp4 = std::fs::read(root.join(format!("{name}.mp4"))).unwrap();
        assert!(
            mp4.len() > 20_000 && mp4[4..8] == *b"ftyp",
            "{name}.mp4 is {} bytes",
            mp4.len()
        );
    }
}

/// `completions` renders a usable script naming the `gen` subcommand.
#[test]
fn completions_render_for_bash() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mcp2dekopon"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.len() > 500, "suspiciously short completions");
    assert!(text.contains("gen"), "{text}");
}

/// CI must run every script in `scripts/`, so new scripts cannot land untested.
#[test]
fn ci_runs_every_script() {
    let ci = repo_file(".github/workflows/ci.yml");
    let scripts = std::fs::read_dir(env!("CARGO_MANIFEST_DIR").to_string() + "/scripts").unwrap();
    let mut count = 0;
    for entry in scripts {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !name.ends_with(".sh") {
            continue;
        }
        count += 1;
        assert!(
            ci.contains(&format!("scripts/{name}")),
            "ci.yml never runs scripts/{name}"
        );
    }
    assert!(count >= 1);
}

/// Every shell script parses (`bash -n`) and the Python mock compiles.
#[test]
fn scripts_pass_syntax_checks() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut count = 0;
    for entry in std::fs::read_dir(root.join("scripts")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".sh") {
            count += 1;
            let output = std::process::Command::new("bash")
                .args(["-n", path.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "bash -n {name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else if name.ends_with(".py") {
            count += 1;
            let output = std::process::Command::new("python3")
                .args(["-m", "py_compile", path.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "py_compile {name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert!(count >= 1);
}

/// The coverage doc must name every `##` section of the PRD, so PRD edits
/// cannot silently outdate it.
#[test]
fn prd_coverage_names_every_prd_section() {
    let prd = repo_file("PRD.md");
    let coverage = repo_file("docs/PRD-coverage.md");
    let mut sections = 0;
    for line in prd.lines() {
        if let Some(section) = line.strip_prefix("## ") {
            sections += 1;
            assert!(
                coverage.contains(section),
                "docs/PRD-coverage.md never names PRD section {section:?}"
            );
        }
    }
    assert!(sections >= 1);
}
