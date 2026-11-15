//! Deterministic request-authorization view of a deployment-shaped policy.
//!
//! A task policy is written once, in the shape every consumer can read: bare
//! `principal` headers plus `context.input.operation` where an operation-level
//! restriction is needed. Sandbox projection and the `ActPlane` translator consume
//! that text unchanged. The daemon PDP answers "may this launch start" before the
//! command runs, so it cannot read per-operation context and only accepts
//! explicit principals. This module derives that second view from the same
//! revision, without asking the model for a second document and without
//! persisting a second policy.

use asc_policy_types::cedar::CedarPolicySource;

use crate::CedarError;

const VIEW_HEADER: &str =
    "// Derived request-authorization view: permits without runtime context only.\n";

/// Builds the launch-time view of one stored policy revision.
///
/// The view keeps only `permit` statements that never mention `context`, and
/// rewrites a bare `principal` header to `principal is SecCore::Subject`. Every
/// `forbid` is dropped: a `forbid` without context would be enforced before the
/// sandbox even starts, and one with context cannot be evaluated at launch, so
/// operation-level denials stay with the deployment-backed enforcement path.
///
/// An empty result means the revision grants nothing at launch time; callers
/// leave it out of the authorization set instead of treating it as a deny-all
/// policy.
///
/// # Errors
/// Returns the same admission errors as [`crate::validate`] when the derived
/// text is not valid Cedar, or [`CedarError::Unsupported`] when a kept statement
/// has no recognizable `principal` header.
pub fn authorization_view(source: &CedarPolicySource) -> Result<CedarPolicySource, CedarError> {
    let mut kept = Vec::new();
    for statement in statements(&source.text) {
        let code = code_without_comments(statement);
        if effect(&code) != Some("permit") || mentions_context(&code) {
            continue;
        }
        // Comments leave blank lines behind, so each kept statement is trimmed before joining.
        kept.push(explicit_principal(statement)?.trim_end().to_owned());
    }
    if kept.is_empty() {
        return Ok(CedarPolicySource {
            schema: source.schema.clone(),
            text: String::new(),
        });
    }
    let text = format!("{VIEW_HEADER}\n{}\n", kept.join("\n\n"));
    let view = CedarPolicySource {
        schema: source.schema.clone(),
        text,
    };
    crate::validate(&view)?;
    Ok(view)
}

/// Reports whether a source renders at least one launch-time statement.
///
/// [`authorization_view`] returns an empty source when nothing survives; callers
/// use this to skip the member instead of publishing a policy that grants
/// nothing.
#[must_use]
pub fn has_statements(source: &CedarPolicySource) -> bool {
    !source.text.trim().is_empty()
}

/// Splits Cedar text into top-level statements, respecting strings and comments.
fn statements(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut depth = 0_i32;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len()
                    && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth -= 1;
                index += 1;
            }
            b';' if depth <= 0 => {
                let end = (index + 1).min(bytes.len());
                if !text[start..end].trim().is_empty() {
                    found.push(&text[start..end]);
                }
                start = end;
                index = end;
            }
            _ => index += 1,
        }
    }
    found
}

/// Drops comments so effect and identifier checks see code only.
fn code_without_comments(statement: &str) -> String {
    let bytes = statement.as_bytes();
    let mut code: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index += 2,
                    b'"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
            code.extend_from_slice(&bytes[start..index.min(bytes.len())]);
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            code.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&code).into_owned()
}

/// Matches `context` as an identifier, so a path segment cannot fake a condition.
fn mentions_context(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                if &code[start..index] == "context" {
                    return true;
                }
            }
            _ => index += 1,
        }
    }
    false
}

/// Returns the effect keyword of a statement's code region.
///
/// The scan starts after any leading comment block: a statement slice carries the
/// comments written above it, and those comments routinely contain the words
/// `permit` and `principal`, which must not be mistaken for the policy header.
fn effect(code: &str) -> Option<&str> {
    identifiers(code)
        .into_iter()
        .map(|(start, end)| &code[start..end])
        .find(|token| matches!(*token, "permit" | "forbid"))
}

/// Byte ranges of every identifier in `code`, skipping string literals.
fn identifiers(code: &str) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' {
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index += 2,
                    b'"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            found.push((start, index));
        } else {
            index += 1;
        }
    }
    found
}

/// Rewrites a bare `principal` header without touching the statement body.
fn explicit_principal(statement: &str) -> Result<String, CedarError> {
    let unsupported = || CedarError::Unsupported {
        policy_id: "policy-set".into(),
        reason: "request authorization view needs a recognizable principal header",
    };
    let code = code_without_comments(statement);
    // Comments leave blank lines behind; the view starts at the effect keyword.
    let code = code.trim_start().to_owned();
    let tokens = identifiers(&code);
    let effect = tokens
        .iter()
        .position(|(start, end)| matches!(&code[*start..*end], "permit" | "forbid"))
        .ok_or_else(unsupported)?;
    let principal = tokens[effect..]
        .iter()
        .find(|(start, end)| &code[*start..*end] == "principal")
        .ok_or_else(unsupported)?;
    // Offsets index the comment-free projection, so the rewrite is rebuilt from
    // that projection and the original comments are dropped from the view.
    let tail = principal.1;
    let rest = code[tail..].trim_start();
    if !rest.starts_with(',') {
        // Already constrained: `principal is ...` or `principal == ...`.
        return Ok(format!("{code}\n"));
    }
    Ok(format!("{} is SecCore::Subject{}", &code[..tail], rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> CedarPolicySource {
        CedarPolicySource {
            schema: crate::schema::embedded_reference(),
            text: text.to_owned(),
        }
    }

    const UNIFIED: &str = r#"// Sandbox projects the [S*] permits; the bare `principal` header is deliberate.
// ============================================================
// Sandbox Policy - resource access boundary
// ============================================================
permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {
  resource.path == "/srv/app" || resource.path like "/srv/app/*"
};

permit(
  principal,
  action == SecCore::Action::"file.write",
  resource is SecCore::File
)
when {
  resource.path == "/srv/app/out" || resource.path like "/srv/app/out/*"
};

// ============================================================
// OS Harness Policy - operation-level protection
// ============================================================
forbid(
  principal,
  action == SecCore::Action::"file.write",
  resource is SecCore::File
)
when {
  resource.path == "/srv/app/releases/previous.zip" &&
  context.input.operation == SecCore::WriteOperation::"delete"
};

forbid(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {
  resource.path == "/srv/app/project/deploy.env"
};
"#;

    #[test]
    fn keeps_context_free_permits_and_drops_every_forbid() {
        let view = authorization_view(&source(UNIFIED)).unwrap();
        assert!(view.text.contains(r#""file.read""#));
        assert!(view.text.contains(r#""file.write""#));
        assert!(!view.text.contains("forbid"));
        assert!(!view.text.contains("context."));
        assert!(!view.text.contains("deploy.env"));
        assert!(crate::request_authorization_eligible(&view).unwrap());
    }

    #[test]
    fn rewrites_bare_principal_exactly_once() {
        let view = authorization_view(&source(UNIFIED)).unwrap();
        assert_eq!(
            view.text.matches("principal is SecCore::Subject").count(),
            2
        );
        assert!(!view.text.contains("principal,"));
    }

    #[test]
    fn comment_words_do_not_steer_the_header_rewrite() {
        // The keywords in a leading comment must not be taken for the effect or header.
        let text = r#"// This comment says permit and principal before any code.
permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
);
"#;
        let view = authorization_view(&source(text)).unwrap();
        assert_eq!(
            view.text.matches("principal is SecCore::Subject").count(),
            1
        );
        assert!(crate::request_authorization_eligible(&view).unwrap());
    }

    #[test]
    fn keeps_explicit_principal_unchanged() {
        let text = r#"permit(
  principal is SecCore::Subject,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
);
"#;
        let view = authorization_view(&source(text)).unwrap();
        assert_eq!(
            view.text.matches("principal is SecCore::Subject").count(),
            1
        );
    }

    #[test]
    fn empty_when_only_context_rules_exist() {
        let text = r#"forbid(
  principal,
  action == SecCore::Action::"file.write",
  resource is SecCore::File
)
when {
  context.input.operation == SecCore::WriteOperation::"delete"
};
"#;
        let view = authorization_view(&source(text)).unwrap();
        assert!(!has_statements(&view));
    }

    #[test]
    fn comment_mentions_do_not_drop_a_permit() {
        let text = r#"// context.input.operation is handled by the enforcement path
permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {
  resource.path == "/srv/app" || resource.path like "/srv/app/*"
};
"#;
        let view = authorization_view(&source(text)).unwrap();
        assert!(view.text.contains("file.read"));
    }

    #[test]
    fn semicolon_inside_string_does_not_split_statements() {
        let text = r#"permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {
  resource.path == "/srv/a;b" || resource.path like "/srv/a;b/*"
};
"#;
        let view = authorization_view(&source(text)).unwrap();
        assert!(has_statements(&view));
        assert!(view.text.contains("/srv/a;b/*"));
    }
}
