//! Contract test for the cosh extension PreToolUse rewrite matcher.
//!
//! The matcher string shipped in `adapters/tokenless/common/cosh-extension.json`
//! is compiled at runtime by cosh-ng's hook system (`HookSystem::matches_tool`)
//! with the Rust `regex` crate and applied as an *unanchored*
//! `Regex::is_match` search (≈ Python's `re.search`). A matcher that fails
//! `Regex::new` does not raise an error in cosh-ng: it silently degrades to
//! exact string comparison, which an anchored alternation never satisfies, so
//! the rewrite hook never fires and rtk rewriting is skipped.
//!
//! The Python twin of this contract (`test_cosh_extension_matcher.py` in the
//! tokenless test suite) pins the matcher against Python's `re`, which accepts
//! syntax the Rust `regex` crate rejects (e.g. lookahead). Only this side of
//! the contract exercises the engine that actually compiles the matcher; both
//! sides draw their tool-name corpus from the shared
//! `tests/data/cosh_matcher_corpus.json`.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

// Embedded at compile time: if the shared corpus moves, the build fails
// loudly instead of the test silently weakening.
const MATCHER_CORPUS: &str = include_str!("../../../tests/data/cosh_matcher_corpus.json");

// Resolved against the tokenless root rather than this crate's nesting depth,
// so relocating the crate inside the workspace cannot break the contract.
const MANIFEST_RELATIVE: &str = "adapters/tokenless/common/cosh-extension.json";

fn manifest_path() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = dir.join(MANIFEST_RELATIVE);
        if candidate.is_file() {
            return candidate;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => panic!(
                "cosh-extension.json not found at {MANIFEST_RELATIVE} \
                 in any ancestor of {}",
                env!("CARGO_MANIFEST_DIR")
            ),
        }
    }
}

fn corpus_tools(key: &str) -> Vec<String> {
    let corpus: Value =
        serde_json::from_str(MATCHER_CORPUS).expect("shared matcher corpus must be valid JSON");
    corpus
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("shared matcher corpus must contain a {key:?} array"))
        .iter()
        .map(|entry| {
            entry.as_str().map(str::to_string).unwrap_or_else(|| {
                panic!("entries of {key:?} in the shared corpus must be strings")
            })
        })
        .collect()
}

// Mirrors the lookup in the Python suite's `_rewrite_matcher`, so both sides
// of the contract select the same hook group from the manifest.
fn rewrite_matcher() -> String {
    let path = manifest_path();
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cosh-extension.json must be readable at {}: {err}",
            path.display()
        )
    });
    let manifest: Value =
        serde_json::from_str(&raw).expect("cosh-extension.json must be valid JSON");
    let groups = manifest
        .pointer("/hooks/PreToolUse")
        .and_then(Value::as_array)
        .expect("hooks.PreToolUse must be an array of matcher groups");
    for group in groups {
        let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        let is_rewrite = hooks
            .iter()
            .any(|hook| hook.get("name").and_then(Value::as_str) == Some("tokenless-rewrite"));
        if !is_rewrite {
            continue;
        }
        let matcher = group.get("matcher").and_then(Value::as_str).expect(
            "tokenless-rewrite group must declare an explicit string matcher \
             so non-shell tools never reach rtk",
        );
        assert!(
            !matcher.is_empty(),
            "an empty matcher matches every tool, including non-shell tools"
        );
        return matcher.to_string();
    }
    panic!("tokenless-rewrite hook not found in PreToolUse groups");
}

#[test]
fn matcher_compiles_with_rust_regex_crate() {
    // The gap this contract closes: cosh-ng compiles the matcher with
    // Regex::new. Python's `re` accepts patterns (e.g. lookahead) that this
    // engine rejects, so a matcher can pass the Python tests yet fail here
    // and silently disable the hook in cosh-ng.
    let matcher = rewrite_matcher();
    Regex::new(&matcher).unwrap_or_else(|err| {
        panic!(
            "matcher {matcher:?} must be valid Rust regex syntax \
             (cosh-ng compiles it with Regex::new): {err}"
        )
    });
}

#[test]
// The invalid literal is the point of this test, so opt out of the lint
// that flags invalid regex literals.
#[allow(clippy::invalid_regex)]
fn rust_regex_rejects_python_only_syntax() {
    // Sanity check for the contract itself: lookahead is valid Python `re`
    // syntax but unsupported by the Rust `regex` crate, proving that only
    // this Rust-side test can catch such a matcher.
    assert!(Regex::new("(?=shell)").is_err());
}

#[test]
fn matcher_hits_cosh_shell_tool_name() {
    // The regression from the original fix: cosh-ng names its shell tool
    // `shell`, and the matcher must hit it directly without relying on
    // host-side tool-name aliasing.
    let re = Regex::new(&rewrite_matcher()).expect("matcher must compile");
    assert!(
        re.is_match("shell"),
        "matcher must match cosh-ng's lowercase 'shell' tool name directly"
    );
}

#[test]
fn matcher_hits_all_shell_family_names() {
    let re = Regex::new(&rewrite_matcher()).expect("matcher must compile");
    for name in corpus_tools("matching_tools") {
        assert!(
            re.is_match(&name),
            "matcher must match shell-family tool name {name:?}"
        );
    }
}

#[test]
fn matcher_rejects_non_shell_tools() {
    let re = Regex::new(&rewrite_matcher()).expect("matcher must compile");
    for name in corpus_tools("non_matching_tools") {
        assert!(
            !re.is_match(&name),
            "matcher must not match non-shell tool name {name:?}"
        );
    }
}
