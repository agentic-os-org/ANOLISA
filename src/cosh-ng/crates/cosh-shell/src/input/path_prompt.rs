//! Conservative classification for natural-language submissions whose first
//! shell token contains a path separator.

use std::collections::HashSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use nix::libc;

use super::InterceptReason;

/// Latest physical cwd reported at a primary Enhanced prompt.
///
/// The OSC parser updates this snapshot and the submit-time classifier reads
/// it before routing. Native sessions never publish a value. Logical `PWD`
/// is not accepted because users may reassign it without changing the child
/// process working directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct ShellPromptCwd(Arc<Mutex<Option<String>>>);

impl ShellPromptCwd {
    pub(crate) fn set(&self, cwd: Option<String>) {
        let cwd = cwd
            .filter(|value| Path::new(value).is_absolute() && !value.chars().any(char::is_control));
        if let Ok(mut current) = self.0.lock() {
            *current = cwd;
        }
    }

    pub(crate) fn current(&self) -> Option<String> {
        self.0.lock().ok().and_then(|current| current.clone())
    }
}

impl PartialEq for ShellPromptCwd {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.current() == other.current()
    }
}

impl Eq for ShellPromptCwd {}

/// Slash-bearing command names and suffix aliases Zsh resolves without a
/// filesystem path.
///
/// Prompt markers refresh this snapshot after the shell has completed its
/// own parsing and startup hooks. An unavailable snapshot is intentionally
/// not treated as proof that a name is missing.
#[derive(Debug, Default)]
struct ShellPathNamespace {
    names: HashSet<String>,
    suffixes: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ShellPathCommandNames(Arc<Mutex<Option<ShellPathNamespace>>>);

impl ShellPathCommandNames {
    pub(crate) fn set(&self, names: Option<Vec<String>>, suffixes: Option<Vec<String>>) {
        if let Ok(mut current) = self.0.lock() {
            *current = names
                .zip(suffixes)
                .map(|(names, suffixes)| ShellPathNamespace {
                    names: HashSet::from_iter(names),
                    suffixes: HashSet::from_iter(suffixes),
                });
        }
    }

    pub(crate) fn excludes_first_token(&self, input: &str) -> bool {
        let first_token = input
            .trim_matches([' ', '\t'])
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        self.0
            .lock()
            .ok()
            .and_then(|namespace| {
                namespace.as_ref().map(|namespace| {
                    !namespace.names.contains(first_token)
                        && Path::new(first_token)
                            .extension()
                            .and_then(|suffix| suffix.to_str())
                            .is_none_or(|suffix| !namespace.suffixes.contains(suffix))
                })
            })
            .unwrap_or(false)
    }
}

impl PartialEq for ShellPathCommandNames {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ShellPathCommandNames {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathPromptIntercept {
    pub(crate) input: String,
    pub(crate) reason: InterceptReason,
    pub(crate) cwd: String,
}

pub(crate) fn is_slash_bearing_han_prompt(input: &str, cwd: Option<&Path>) -> bool {
    let input = input.trim_matches([' ', '\t']);
    if input.is_empty() || input.len() > 4096 || !contains_han(input) {
        return false;
    }
    let first_token = input.split_ascii_whitespace().next().unwrap_or_default();
    if !first_token.contains('/') || first_token.starts_with("~/") {
        return false;
    }
    // ENOENT is only meaningful for the literal command word. Quoting,
    // escaping, expansion, and glob syntax can make Readline text resolve to
    // a different path after shell parsing, so those shapes stay Shell-owned.
    if first_token.chars().any(|ch| {
        ch.is_control()
            || matches!(
                ch,
                '\'' | '"'
                    | '\\'
                    | '$'
                    | '`'
                    | '|'
                    | '&'
                    | ';'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '~'
            )
    }) {
        return false;
    }
    if !path_provably_missing(first_token, cwd) {
        return false;
    }

    command_shape_allows_han_prompt(input, contains_han(first_token))
}

fn contains_han(input: &str) -> bool {
    input.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af
        )
    })
}

fn command_shape_allows_han_prompt(input: &str, first_token_has_han: bool) -> bool {
    let scan = input
        .strip_suffix('?')
        .or_else(|| input.strip_suffix('？'))
        .unwrap_or(input);
    if first_token_has_han {
        return han_leading_shape_is_safe(scan);
    }
    if scan.chars().any(|ch| {
        ch.is_control()
            || matches!(
                ch,
                '\'' | '"'
                    | '\\'
                    | '|'
                    | '&'
                    | ';'
                    | '<'
                    | '>'
                    | '$'
                    | '`'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '*'
                    | '?'
                    | '？'
                    | '~'
            )
    }) {
        return false;
    }
    !scan.split_ascii_whitespace().any(|word| {
        word.starts_with('-')
            || word
                .split_once('=')
                .is_some_and(|(name, _)| is_shell_name(name))
    })
}

fn han_leading_shape_is_safe(input: &str) -> bool {
    let chars = input.chars().collect::<Vec<_>>();
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        index += 1;
        if ch.is_control() {
            return false;
        }
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\'' if quote.is_none() => quote = Some('\''),
            '"' => {
                quote = if quote == Some('"') { None } else { Some('"') };
            }
            '(' | ')' | '|' | '&' | ';' | '<' | '>' if quote.is_none() => return false,
            '`' => return false,
            '$' if quote != Some('\'') && !simple_parameter_follows(&chars[index..]) => {
                return false;
            }
            _ => {}
        }
    }
    !escaped && quote.is_none()
}

fn simple_parameter_follows(chars: &[char]) -> bool {
    match chars.first() {
        Some(ch) if ch.is_ascii_alphabetic() || *ch == '_' => true,
        Some('{') => {
            let Some(end) = chars.iter().position(|ch| *ch == '}') else {
                return false;
            };
            let name = &chars[1..end];
            !name.is_empty()
                && (name[0].is_ascii_alphabetic() || name[0] == '_')
                && name[1..]
                    .iter()
                    .all(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        }
        _ => false,
    }
}

fn is_shell_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(crate) fn path_provably_missing(path: &str, cwd: Option<&Path>) -> bool {
    let path = Path::new(path);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let Some(cwd) = cwd else {
            return false;
        };
        // Prompt markers publish the shell builtin's physical cwd. Resolve
        // that trusted prefix once as a liveness check, then keep the user
        // suffix lexical so its own symlinks still fail closed.
        let Ok(cwd) = cwd.canonicalize() else {
            return false;
        };
        cwd.join(path)
    };
    let components = candidate.components().collect::<Vec<_>>();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(_) => return false,
            Component::RootDir => current.push(Path::new("/")),
            Component::CurDir => continue,
            Component::ParentDir => current.push(".."),
            Component::Normal(value) => current.push(value),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return false;
                }
                if index + 1 < components.len() && !metadata.is_dir() {
                    return false;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return current
                    .parent()
                    .is_some_and(parent_is_readable_and_searchable);
            }
            Err(_) => return false,
        }
    }
    false
}

fn parent_is_readable_and_searchable(parent: &Path) -> bool {
    let Ok(parent) = CString::new(parent.as_os_str().as_bytes()) else {
        return false;
    };
    // Match the former shell `-r && -x` ENOENT proof. cosh-shell is not
    // set-id, so real and effective credentials are equivalent here. Any
    // ACL, permission, or lookup uncertainty stays Shell-owned.
    unsafe { libc::access(parent.as_ptr(), libc::R_OK | libc::X_OK) == 0 }
}
