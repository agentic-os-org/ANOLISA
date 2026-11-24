//! Semantic validation rules (R1–R15) for instrumentation profiles.

use super::{Codec, FieldBinding, Fields, InstrumentationTarget, Origin, Runtime, VERSION};
use crate::{
    require,
    source::{is_hook_event, FailureAction, HookAuthority, HookEffect},
    Error,
};
use std::collections::BTreeSet;

const MAX_TARGETS: usize = 32;
const MAX_STREAM_ITEMS: u32 = 65_536;
const MAX_STREAM_BYTES: u32 = 16_777_216;
const MAX_ARG_INDEX: u16 = 31;
const MAX_PATH_INDEX: u16 = 1_023;
const MAX_PATH_SEGMENTS: usize = 6;
const MAX_PATH_LEN: usize = 256;

const NON_INSTRUMENTABLE_EVENTS: [&str; 3] =
    ["runtime.observed", "runtime.exited", "coverage.changed"];

pub(super) fn validate_profile(fields: &Fields) -> Result<(), Error> {
    require(
        fields.version == VERSION,
        "unsupported instrumentation profile version",
    )?;
    require(
        valid_identifier(&fields.id),
        "invalid instrumentation profile identifier",
    )?;
    match fields.runtime {
        Runtime::Python => require(
            valid_python_package_name(&fields.package.name),
            "invalid python package name",
        )?,
        Runtime::Js => require(
            valid_js_package_name(&fields.package.name),
            "invalid js package name",
        )?,
    }
    require(
        valid_version_range(&fields.package.versions),
        "invalid package version comparator range",
    )?;
    require(
        !fields.targets.is_empty() && fields.targets.len() <= MAX_TARGETS,
        "instrumentation profile requires between 1 and 32 targets",
    )?;

    let mut seen_ids = BTreeSet::new();
    let mut seen_seams = BTreeSet::new();
    for target in &fields.targets {
        require(
            valid_identifier(&target.id),
            "invalid instrumentation target identifier",
        )?;
        require(
            seen_ids.insert(target.id.as_str()),
            "duplicate instrumentation target identifier",
        )?;
        require(
            seen_seams.insert((target.module.as_str(), target.qualname.as_str())),
            "duplicate instrumentation target (module, qualname) seam",
        )?;
        validate_target(fields.origin, fields.runtime, &fields.package.name, target)?;
    }
    Ok(())
}

fn validate_target(
    origin: Origin,
    runtime: Runtime,
    package_name: &str,
    target: &InstrumentationTarget,
) -> Result<(), Error> {
    match runtime {
        Runtime::Python => require(
            valid_python_module(&target.module),
            "invalid python module specifier",
        )?,
        Runtime::Js => {
            require(
                valid_js_module(&target.module),
                "invalid js package module specifier",
            )?;
            require(
                target.module == package_name
                    || target
                        .module
                        .strip_prefix(package_name)
                        .is_some_and(|rest| rest.starts_with('/')),
                "js target module must belong to the declared package",
            )?;
        }
    }
    require(
        valid_qualname(&target.qualname),
        "invalid target qualified name",
    )?;
    require(
        !target.hooks.is_empty(),
        "instrumentation target requires at least one Hook",
    )?;

    let kind = runtime.source_kind();
    let mut allowed_fields = BTreeSet::from(["session_id", "turn_id"]);
    for (event, cap) in &target.hooks {
        require(is_hook_event(event), "unsupported semantic Hook event")?;
        require(
            !NON_INSTRUMENTABLE_EVENTS.contains(&event.as_str()),
            "AW-produced events are not instrumentable",
        )?;
        cap.validate_for_event(kind, event)?;
        match event.as_str() {
            "tool.before" | "permission.request" => {
                allowed_fields.extend(["tool_name", "call_id", "input"]);
            }
            "tool.after" => {
                allowed_fields.extend(["tool_name", "call_id", "input", "result"]);
            }
            "input.submit" => {
                allowed_fields.insert("prompt");
            }
            "turn.stop" | "security.violation" => {
                allowed_fields.insert("output");
            }
            "model.before_request" => {
                allowed_fields.insert("request");
            }
            _ => {}
        }
    }

    for (field_name, binding) in &target.fields {
        require(
            allowed_fields.contains(field_name.as_str()),
            "field is not allowed for the target's declared Hooks",
        )?;
        match binding {
            FieldBinding::Const { value } => {
                require(
                    field_name == "tool_name",
                    "const field binding is only allowed for tool_name",
                )?;
                require(
                    valid_profile_string(value),
                    "const field binding must be a non-blank profile string",
                )?;
            }
            FieldBinding::From { from, .. } => {
                let root = parse_path_root(runtime, from)?;
                if root == PathRoot::Result {
                    require(
                        matches!(field_name.as_str(), "result" | "output"),
                        "result path root is only allowed for result and output fields",
                    )?;
                }
            }
        }
    }

    validate_required_fields(runtime, target)?;
    validate_stream_and_fingerprint(origin, runtime, target)
}

fn validate_required_fields(runtime: Runtime, target: &InstrumentationTarget) -> Result<(), Error> {
    if let Some(before) = target.hooks.get("tool.before") {
        require(
            target.fields.contains_key("tool_name") && target.fields.contains_key("input"),
            "tool.before target requires tool_name and input fields",
        )?;
        if before.supports_effect(HookEffect::ReplaceInput) {
            let input = target
                .fields
                .get("input")
                .ok_or(Error::Invariant("tool.before target requires input field"))?;
            require(
                matches!(input.codec(), Some(Codec::Json | Codec::JsonString)),
                "tool.before replace_input requires input codec json or json_string",
            )?;
        }
    }

    if let Some(after) = target.hooks.get("tool.after") {
        require(
            target.fields.contains_key("tool_name") && target.fields.contains_key("result"),
            "tool.after target requires tool_name and result fields",
        )?;
        if after.supports_effect(HookEffect::ReplaceResult) {
            let result_field = target
                .fields
                .get("result")
                .ok_or(Error::Invariant("tool.after target requires result field"))?;
            let path = result_field.path().ok_or(Error::Invariant(
                "tool.after replace_result requires result path root",
            ))?;
            require(
                parse_path_root(runtime, path)? == PathRoot::Result,
                "tool.after replace_result requires result path root",
            )?;
        }
    }

    if target.hooks.contains_key("permission.request") {
        require(
            target.fields.contains_key("tool_name"),
            "permission.request target requires tool_name field",
        )?;
    }

    if target.hooks.contains_key("input.submit") {
        require(
            target.fields.contains_key("prompt"),
            "input.submit target requires prompt field",
        )?;
    }

    if target.hooks.contains_key("security.violation") {
        require(
            target.fields.contains_key("output"),
            "security.violation target requires output field",
        )?;
    }

    Ok(())
}

fn validate_stream_and_fingerprint(
    origin: Origin,
    runtime: Runtime,
    target: &InstrumentationTarget,
) -> Result<(), Error> {
    if let Some(stream) = &target.stream {
        require(
            target.call.is_iter(),
            "stream configuration is only allowed for iterator call shapes",
        )?;
        require(
            (1..=MAX_STREAM_ITEMS).contains(&stream.max_items),
            "stream max_items must be between 1 and 65536",
        )?;
        require(
            (1..=MAX_STREAM_BYTES).contains(&stream.max_bytes),
            "stream max_bytes must be between 1 and 16777216",
        )?;
    }

    if target.call.is_iter() {
        let needs_buffered_stream = target.hooks.get("tool.after").is_some_and(|after| {
            after.supports_effect(HookEffect::ReplaceResult)
                || after
                    .failure_actions
                    .contains(&FailureAction::WithholdResult)
        });
        if needs_buffered_stream {
            require(
                target.stream.as_ref().is_some_and(|s| s.buffer),
                "iterator tool.after with replace_result or withhold_result requires buffered stream",
            )?;
        }
        if target
            .hooks
            .get("security.violation")
            .is_some_and(|gate| gate.mode == HookAuthority::Control)
        {
            if let Some(path) = target.fields.get("output").and_then(|field| field.path()) {
                if parse_path_root(runtime, path)? == PathRoot::Result {
                    require(
                        target.stream.as_ref().is_some_and(|s| s.buffer),
                        "iterator security.violation gate reading result requires buffered stream",
                    )?;
                }
            }
        }
    }

    if let Some(sha) = &target.code_sha256 {
        require(
            valid_sha256(sha),
            "target code_sha256 must be 64 lowercase hexadecimal characters",
        )?;
    } else if origin == Origin::Custom {
        return Err(Error::Invariant(
            "custom instrumentation profile targets require code_sha256",
        ));
    }

    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathRoot {
    Receiver,
    Arg,
    Kwarg,
    Result,
}

fn parse_path_root(runtime: Runtime, path: &str) -> Result<PathRoot, Error> {
    require(
        !path.is_empty() && path.len() <= MAX_PATH_LEN,
        "field extraction path must be 1 to 256 bytes",
    )?;
    let parts: Vec<&str> = path.split('.').collect();
    let (root, trailing) = match parts.first().copied() {
        Some("receiver") => (PathRoot::Receiver, &parts[1..]),
        Some("result") => (PathRoot::Result, &parts[1..]),
        Some("arg") => {
            let index = parts
                .get(1)
                .copied()
                .ok_or(Error::Invariant("arg path root requires an index 0..31"))?;
            require(
                parse_canonical_index(index, MAX_ARG_INDEX).is_some(),
                "arg path index must be a decimal integer in 0..31",
            )?;
            (PathRoot::Arg, &parts[2..])
        }
        Some("kwarg") => {
            require(
                runtime == Runtime::Python,
                "kwarg path root is only supported for python runtime",
            )?;
            let name = parts
                .get(1)
                .copied()
                .ok_or(Error::Invariant("kwarg path root requires an identifier"))?;
            require(
                valid_non_dunder_ident(name),
                "kwarg path name must be a non-dunder identifier",
            )?;
            (PathRoot::Kwarg, &parts[2..])
        }
        _ => return Err(Error::Invariant("unsupported field extraction path root")),
    };

    require(
        trailing.len() <= MAX_PATH_SEGMENTS,
        "field extraction path allows at most 6 trailing segments",
    )?;
    for segment in trailing {
        let valid = valid_non_dunder_ident(segment)
            || parse_canonical_index(segment, MAX_PATH_INDEX).is_some();
        require(
            valid,
            "field extraction path segment must be a non-dunder identifier or index 0..1023",
        )?;
    }
    Ok(root)
}

fn parse_canonical_index(segment: &str, max: u16) -> Option<u16> {
    if segment.is_empty() || !segment.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if segment.len() > 1 && segment.starts_with('0') {
        return None;
    }
    let value: u16 = segment.parse().ok()?;
    (value <= max).then_some(value)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_profile_string(value: &str) -> bool {
    let char_count = value.chars().count();
    (1..=128).contains(&char_count)
        && value.chars().any(|ch| !ch.is_whitespace())
        && !value
            .chars()
            .any(|ch| matches!(ch, '\r' | '\n' | '\0' | '\u{2028}' | '\u{2029}'))
}

fn valid_python_ident(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

fn valid_non_dunder_ident(segment: &str) -> bool {
    valid_python_ident(segment) && !segment.starts_with("__")
}

fn valid_python_package_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_js_name_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let is_lower_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !is_lower_alnum(bytes[0]) {
        return false;
    }
    bytes
        .iter()
        .all(|&b| is_lower_alnum(b) || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

fn valid_js_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    if let Some(rest) = name.strip_prefix('@') {
        let Some((scope, pkg)) = rest.split_once('/') else {
            return false;
        };
        !pkg.contains('/') && valid_js_name_segment(scope) && valid_js_name_segment(pkg)
    } else {
        !name.contains('/') && valid_js_name_segment(name)
    }
}

fn valid_version_range(versions: &str) -> bool {
    if versions.is_empty() || versions.len() > 128 {
        return false;
    }
    let mut count = 0usize;
    for comparator in versions.split(',') {
        count += 1;
        if count > 8 {
            return false;
        }
        let rest = comparator
            .strip_prefix(">=")
            .or_else(|| comparator.strip_prefix("<="))
            .or_else(|| comparator.strip_prefix("=="))
            .or_else(|| comparator.strip_prefix('>'))
            .or_else(|| comparator.strip_prefix('<'));
        let Some(version) = rest else {
            return false;
        };
        let mut parts = 0usize;
        for part in version.split('.') {
            parts += 1;
            if parts > 4 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
        }
        if parts == 0 {
            return false;
        }
    }
    count >= 1
}

fn valid_python_module(module: &str) -> bool {
    !module.is_empty() && module.len() <= 256 && module.split('.').all(valid_non_dunder_ident)
}

fn valid_js_subpath_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_js_module(module: &str) -> bool {
    if module.is_empty() || module.len() > 256 {
        return false;
    }
    if module.starts_with('@') {
        let mut iter = module.split('/');
        let (Some(scope), Some(pkg)) = (iter.next(), iter.next()) else {
            return false;
        };
        let pkg_len = scope.len() + 1 + pkg.len();
        pkg_len <= 214
            && valid_js_name_segment(&scope[1..])
            && valid_js_name_segment(pkg)
            && iter.all(valid_js_subpath_segment)
    } else {
        let mut iter = module.split('/');
        let Some(pkg) = iter.next() else {
            return false;
        };
        valid_js_package_name(pkg) && iter.all(valid_js_subpath_segment)
    }
}

fn valid_qualname(qualname: &str) -> bool {
    if qualname.is_empty() || qualname.len() > 256 {
        return false;
    }
    let mut count = 0usize;
    for segment in qualname.split('.') {
        count += 1;
        if count > 4 || !valid_non_dunder_ident(segment) {
            return false;
        }
    }
    count >= 1
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
