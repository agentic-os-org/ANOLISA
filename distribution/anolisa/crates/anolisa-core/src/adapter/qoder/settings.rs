//! Qoder `settings.json` merge, prune, and verification helpers.

use std::path::Path;

use serde_json::{Map, Value};

use crate::adapter::AdapterError;
use crate::adapter::claim::QoderManagedHook;
use crate::adapter::driver::DriverCtx;

use super::{HOOKS_PLACEHOLDER, QODER_HOOKS_FILE, common_hooks_dir, plugin_entry};

/// Outcome of reading and inspecting `settings.json` for `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsProbe {
    /// The file exists and parses; whether our two managed entries are set.
    Present {
        hooks_present: bool,
        plugin_enabled: bool,
    },
    /// The file does not exist.
    Absent,
    /// The file cannot be read or parsed, so absence must not be inferred.
    Unverifiable,
}

/// Read `settings` through controlled ops and report managed-entry presence.
pub(super) fn probe_settings(
    ctx: &DriverCtx,
    settings: &Path,
    managed_hooks: &[QoderManagedHook],
    plugin_entry: &str,
) -> SettingsProbe {
    match ctx.ops.read_file(settings) {
        Ok(None) => SettingsProbe::Absent,
        Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(root)) => {
                let (hooks_present, plugin_enabled) =
                    settings_managed_present(&root, managed_hooks, plugin_entry);
                SettingsProbe::Present {
                    hooks_present,
                    plugin_enabled,
                }
            }
            Ok(_) => SettingsProbe::Present {
                hooks_present: false,
                plugin_enabled: false,
            },
            Err(_) => SettingsProbe::Unverifiable,
        },
        Err(_) => SettingsProbe::Unverifiable,
    }
}

/// Prune managed entries from `settings.json` through controlled ops.
pub(super) fn prune_settings_via_ops(
    ctx: &DriverCtx,
    settings: &Path,
    plugin: &str,
    managed_hooks: &[QoderManagedHook],
    messages: &mut Vec<String>,
) -> bool {
    let bytes = match ctx.ops.read_file(settings) {
        Ok(None) => {
            messages.push("~/.qoder/settings.json absent; nothing to prune".to_string());
            return true;
        }
        Ok(Some(bytes)) => bytes,
        Err(err) => {
            messages.push(format!("failed to read settings.json: {err}"));
            return false;
        }
    };
    let mut root = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(root)) => root,
        Ok(_) => {
            messages.push("settings.json is not a JSON object; left untouched".to_string());
            return true;
        }
        Err(err) => {
            messages.push(format!("settings.json unparseable; left untouched: {err}"));
            return false;
        }
    };
    if !prune_managed(&mut root, managed_hooks, &plugin_entry(plugin)) {
        messages.push("settings.json already free of ANOLISA qoder entries".to_string());
        return true;
    }
    let out = match serde_json::to_vec_pretty(&Value::Object(root)) {
        Ok(out) => out,
        Err(err) => {
            messages.push(format!("failed to render pruned settings JSON: {err}"));
            return false;
        }
    };
    match ctx.ops.write_file(settings, &out) {
        Ok(()) => {
            messages.push("pruned ANOLISA qoder entries from settings.json".to_string());
            true
        }
        Err(err) => {
            messages.push(format!("failed to write settings.json: {err}"));
            false
        }
    }
}

/// Parse settings bytes for a merge.
pub(super) fn load_settings_for_merge(
    existing: Option<Vec<u8>>,
    path: &Path,
) -> Result<Map<String, Value>, AdapterError> {
    match existing {
        None => Ok(Map::new()),
        Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(root)) => {
                validate_settings_shape(&root).map_err(|reason| {
                    AdapterError::SettingsUnparseable {
                        path: path.to_path_buf(),
                        reason,
                    }
                })?;
                Ok(root)
            }
            Ok(_) => Err(AdapterError::SettingsUnparseable {
                path: path.to_path_buf(),
                reason: "settings.json must be a JSON object".to_string(),
            }),
            Err(source) => Err(AdapterError::SettingsUnparseable {
                path: path.to_path_buf(),
                reason: source.to_string(),
            }),
        },
    }
}

/// Read `hooks.json` and expand the hook-scripts placeholder to the
/// absolute sibling `common/hooks` path, returning the patched text.
/// Used both for the entries merged into `settings.json` and for the
/// staged plugin copy handed to `qodercli plugins install` — qodercli
/// copies the plugin into its cache verbatim, and consumers that load
/// the cached `hooks.json` directly never expand the placeholder.
pub(super) fn render_resolved_hooks_text(resource_root: &Path) -> Result<String, AdapterError> {
    let path = resource_root.join(QODER_HOOKS_FILE);
    let bytes = std::fs::read(&path).map_err(|source| AdapterError::Io {
        path: path.clone(),
        source,
    })?;
    let hooks_dir = common_hooks_dir(resource_root);
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.replace(HOOKS_PLACEHOLDER, &hooks_dir.to_string_lossy()))
}

/// Read `hooks.json`, expand hook script placeholders, and parse it.
pub(super) fn load_resolved_hooks(resource_root: &Path) -> Result<Value, AdapterError> {
    let substituted = render_resolved_hooks_text(resource_root)?;
    serde_json::from_str(&substituted).map_err(|source| AdapterError::BundleInvalid {
        root: resource_root.to_path_buf(),
        reason: format!("failed to parse {QODER_HOOKS_FILE}: {source}"),
    })
}

/// Every hook name declared in the bundle's `hooks.json`.
pub(super) fn collect_expected_hook_names(
    resource_root: &Path,
) -> Result<Vec<String>, AdapterError> {
    let specs = collect_managed_hook_specs(resource_root)?;
    let mut names = Vec::new();
    for spec in specs {
        if let Some(hs) = spec.entry.get("hooks").and_then(Value::as_array) {
            for h in hs {
                if let Some(name) = h.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
    }
    Ok(names)
}

/// Full managed hook specs declared in the bundle's `hooks.json`.
pub(super) fn collect_managed_hook_specs(
    resource_root: &Path,
) -> Result<Vec<QoderManagedHook>, AdapterError> {
    let resolved = load_resolved_hooks(resource_root)?;
    Ok(resolved_hook_entries(&resolved))
}

/// Add managed hooks and `<plugin>@local` to a settings object.
pub(super) fn merge_managed(
    root: &mut Map<String, Value>,
    managed_hooks: &[QoderManagedHook],
    plugin_entry: &str,
) -> Result<(), String> {
    validate_settings_shape(root)?;
    for spec in managed_hooks {
        let hooks_slot = root
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()));
        let hooks = hooks_slot
            .as_object_mut()
            .expect("settings shape validated: hooks is an object");
        let slot = hooks
            .entry(spec.event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let arr = slot
            .as_array_mut()
            .expect("settings shape validated: hook event is an array");
        let Some(name) = primary_hook_name(&spec.entry) else {
            continue;
        };
        if let Some(pos) = arr
            .iter()
            .position(|existing| primary_hook_name(existing).as_deref() == Some(name.as_str()))
        {
            if arr[pos] != spec.entry {
                arr[pos] = spec.entry.clone();
            }
        } else {
            arr.push(spec.entry.clone());
        }
    }

    let plugins_slot = root
        .entry("plugins")
        .or_insert_with(|| Value::Object(Map::new()));
    let plugins = plugins_slot
        .as_object_mut()
        .expect("settings shape validated: plugins is an object");
    let enabled_slot = plugins
        .entry("enabled")
        .or_insert_with(|| Value::Array(Vec::new()));
    let enabled = enabled_slot
        .as_array_mut()
        .expect("settings shape validated: plugins.enabled is an array");
    if !enabled.iter().any(|v| v.as_str() == Some(plugin_entry)) {
        enabled.push(Value::String(plugin_entry.to_string()));
    }
    Ok(())
}

fn resolved_hook_entries(resolved: &Value) -> Vec<QoderManagedHook> {
    let mut out = Vec::new();
    if let Some(resolved_hooks) = resolved.get("hooks").and_then(Value::as_object) {
        for (event, entries) in resolved_hooks {
            if let Some(entries) = entries.as_array() {
                for entry in entries {
                    if primary_hook_name(entry).is_some() {
                        out.push(QoderManagedHook {
                            event: event.clone(),
                            entry: entry.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

fn prune_managed(
    root: &mut Map<String, Value>,
    managed_hooks: &[QoderManagedHook],
    plugin_entry: &str,
) -> bool {
    let mut removed = false;

    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            if let Some(arr) = hooks.get_mut(&event).and_then(Value::as_array_mut) {
                let before = arr.len();
                arr.retain(|entry| {
                    !managed_hooks
                        .iter()
                        .any(|managed| managed.event == event && entry == &managed.entry)
                });
                if arr.len() != before {
                    removed = true;
                }
                if arr.is_empty() {
                    hooks.remove(&event);
                    removed = true;
                }
            }
        }
        if hooks.is_empty() {
            root.remove("hooks");
        }
    }

    if let Some(plugins) = root.get_mut("plugins").and_then(Value::as_object_mut) {
        if let Some(enabled) = plugins.get_mut("enabled").and_then(Value::as_array_mut) {
            let before = enabled.len();
            enabled.retain(|v| v.as_str() != Some(plugin_entry));
            if enabled.len() != before {
                removed = true;
            }
            if enabled.is_empty() {
                plugins.remove("enabled");
            }
        }
        if plugins.is_empty() {
            root.remove("plugins");
        }
    }

    removed
}

fn settings_managed_present(
    root: &Map<String, Value>,
    managed_hooks: &[QoderManagedHook],
    plugin_entry: &str,
) -> (bool, bool) {
    let hooks_present = !managed_hooks.is_empty()
        && managed_hooks
            .iter()
            .all(|spec| hook_entry_present(root, &spec.event, &spec.entry));
    let plugin_enabled = root
        .get("plugins")
        .and_then(Value::as_object)
        .and_then(|p| p.get("enabled"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(plugin_entry)))
        .unwrap_or(false);
    (hooks_present, plugin_enabled)
}

fn hook_entry_present(root: &Map<String, Value>, event: &str, expected: &Value) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .map(|entries| entries.iter().any(|entry| entry == expected))
        .unwrap_or(false)
}

fn primary_hook_name(entry: &Value) -> Option<String> {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .and_then(|hooks| {
            hooks
                .iter()
                .find_map(|h| h.get("name").and_then(Value::as_str).map(str::to_string))
        })
}

fn validate_settings_shape(root: &Map<String, Value>) -> Result<(), String> {
    if let Some(hooks) = root.get("hooks") {
        let hooks = hooks
            .as_object()
            .ok_or_else(|| "settings.json field 'hooks' must be an object".to_string())?;
        for (event, entries) in hooks {
            if !entries.is_array() {
                return Err(format!("settings.json hooks.{event} must be an array"));
            }
        }
    }
    if let Some(plugins) = root.get("plugins") {
        let plugins = plugins
            .as_object()
            .ok_or_else(|| "settings.json field 'plugins' must be an object".to_string())?;
        if plugins
            .get("enabled")
            .is_some_and(|enabled| !enabled.is_array())
        {
            return Err("settings.json plugins.enabled must be an array".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> Value {
        serde_json::from_str(s).expect("valid JSON")
    }

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("expected object, got {other}"),
        }
    }

    fn resolved_hooks() -> Value {
        json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-rewrite",
                      "command": "python3 /anolisa/rewrite.py" }
                  ] }
                ],
                "PostToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-compress",
                      "command": "python3 /anolisa/compress.py" }
                  ] }
                ]
              }
            }"#,
        )
    }

    fn managed_specs() -> Vec<QoderManagedHook> {
        resolved_hook_entries(&resolved_hooks())
    }

    #[test]
    fn merge_into_empty_adds_hooks_and_plugin() {
        let mut root = Map::new();
        merge_managed(&mut root, &managed_specs(), "tokenless@local").expect("merge");
        let v = Value::Object(root);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["name"],
            "tokenless-rewrite"
        );
        assert_eq!(v["plugins"]["enabled"][0], "tokenless@local");
    }

    #[test]
    fn merge_preserves_user_hooks_and_config() {
        let mut root = obj(json(
            r#"{
              "theme": "dark",
              "hooks": {
                "PreToolUse": [
                  { "hooks": [ { "type": "command", "name": "user-audit" } ] }
                ]
              },
              "plugins": { "enabled": ["other@local"], "registry": "corp" }
            }"#,
        ));
        merge_managed(&mut root, &managed_specs(), "tokenless@local").expect("merge");
        let v = Value::Object(root);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["plugins"]["registry"], "corp");
        let pre = v["hooks"]["PreToolUse"].as_array().expect("array");
        let names: Vec<&str> = pre
            .iter()
            .filter_map(|e| e["hooks"][0]["name"].as_str())
            .collect();
        assert!(names.contains(&"user-audit"));
        assert!(names.contains(&"tokenless-rewrite"));
        let enabled = v["plugins"]["enabled"].as_array().expect("array");
        assert!(enabled.iter().any(|x| x == "other@local"));
        assert!(enabled.iter().any(|x| x == "tokenless@local"));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut root = Map::new();
        merge_managed(&mut root, &managed_specs(), "tokenless@local").expect("merge");
        merge_managed(&mut root, &managed_specs(), "tokenless@local").expect("merge");
        let v = Value::Object(root);
        assert_eq!(
            v["hooks"]["PreToolUse"].as_array().expect("array").len(),
            1,
            "re-merge must not duplicate a hook by name"
        );
        assert_eq!(
            v["plugins"]["enabled"].as_array().expect("array").len(),
            1,
            "re-merge must not duplicate the plugin entry"
        );
    }

    #[test]
    fn merge_replaces_same_named_hook_with_managed_body() {
        let mut root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-rewrite",
                      "command": "python3 /user/rewrite.py" }
                  ] }
                ]
              }
            }"#,
        ));

        merge_managed(&mut root, &managed_specs(), "tokenless@local").expect("merge");

        let v = Value::Object(root);
        let pre = v["hooks"]["PreToolUse"].as_array().expect("array");
        assert_eq!(pre.len(), 1, "same name is replaced, not duplicated");
        assert_eq!(pre[0]["hooks"][0]["command"], "python3 /anolisa/rewrite.py");
    }

    #[test]
    fn prune_removes_only_managed_entries() {
        let mut root = obj(json(
            r#"{
              "theme": "dark",
              "hooks": {
                "PreToolUse": [
                  { "hooks": [ { "type": "command", "name": "user-audit" } ] },
                  { "hooks": [
                    { "type": "command", "name": "tokenless-rewrite",
                      "command": "python3 /anolisa/rewrite.py" }
                  ] }
                ],
                "PostToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-compress",
                      "command": "python3 /anolisa/compress.py" }
                  ] }
                ]
              },
              "plugins": { "enabled": ["other@local", "tokenless@local"] }
            }"#,
        ));
        let changed = prune_managed(&mut root, &managed_specs(), "tokenless@local");
        assert!(changed);
        let v = Value::Object(root);
        assert_eq!(v["theme"], "dark");
        let pre = v["hooks"]["PreToolUse"].as_array().expect("array");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["name"], "user-audit");
        assert!(v["hooks"].get("PostToolUse").is_none());
        let enabled = v["plugins"]["enabled"].as_array().expect("array");
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0], "other@local");
    }

    #[test]
    fn prune_does_not_match_user_hook_by_command_substring() {
        let mut root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [ { "type": "command", "name": "my-hook",
                                 "command": "run tokenless stats" } ] }
                ]
              }
            }"#,
        ));
        let changed = prune_managed(&mut root, &managed_specs(), "tokenless@local");
        assert!(!changed, "no managed entries present");
        let v = Value::Object(root);
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["name"], "my-hook");
    }

    #[test]
    fn prune_does_not_match_user_hook_by_tokenless_prefix() {
        let mut root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-my-custom-audit",
                      "command": "python3 /user/audit.py" }
                  ] }
                ]
              }
            }"#,
        ));
        let changed = prune_managed(&mut root, &managed_specs(), "tokenless@local");
        assert!(!changed, "prefix-only hook is user-owned");
        let v = Value::Object(root);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["name"],
            "tokenless-my-custom-audit"
        );
    }

    #[test]
    fn prune_collapses_emptied_containers_and_is_idempotent() {
        let mut root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-rewrite",
                      "command": "python3 /anolisa/rewrite.py" }
                  ] }
                ]
              },
              "plugins": { "enabled": ["tokenless@local"] }
            }"#,
        ));
        assert!(prune_managed(
            &mut root,
            &managed_specs(),
            "tokenless@local"
        ));
        assert!(root.get("hooks").is_none());
        assert!(root.get("plugins").is_none());
        assert!(!prune_managed(
            &mut root,
            &managed_specs(),
            "tokenless@local"
        ));
    }

    #[test]
    fn prune_with_empty_specs_removes_plugin_entry_only() {
        let mut root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "hooks": [
                    { "type": "command", "name": "tokenless-rewrite",
                      "command": "python3 /user/rewrite.py" }
                  ] }
                ]
              },
              "plugins": { "enabled": ["other@local", "tokenless@local"] }
            }"#,
        ));
        assert!(prune_managed(&mut root, &[], "tokenless@local"));
        let v = Value::Object(root);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["name"],
            "tokenless-rewrite"
        );
        let enabled = v["plugins"]["enabled"].as_array().expect("enabled");
        assert_eq!(enabled, &vec![Value::String("other@local".to_string())]);
    }

    #[test]
    fn settings_managed_present_detects_both_signals() {
        let root = obj(json(
            r#"{
              "hooks": {
                "PreToolUse": [ { "hooks": [
                  { "type": "command", "name": "tokenless-rewrite",
                    "command": "python3 /anolisa/rewrite.py" }
                ] } ],
                "PostToolUse": [ { "hooks": [
                  { "type": "command", "name": "tokenless-compress",
                    "command": "python3 /anolisa/compress.py" }
                ] } ]
              },
              "plugins": { "enabled": ["tokenless@local"] }
            }"#,
        ));
        assert_eq!(
            settings_managed_present(&root, &managed_specs(), "tokenless@local"),
            (true, true)
        );

        let partial = obj(json(r#"{ "plugins": { "enabled": ["tokenless@local"] } }"#));
        assert_eq!(
            settings_managed_present(&partial, &managed_specs(), "tokenless@local"),
            (false, true)
        );

        let none = obj(json(r#"{ "theme": "dark" }"#));
        assert_eq!(
            settings_managed_present(&none, &managed_specs(), "tokenless@local"),
            (false, false)
        );
    }

    #[test]
    fn settings_managed_present_requires_full_expected_hooks() {
        let root = obj(json(
            r#"{
              "hooks": { "PreToolUse": [
                { "hooks": [
                  { "type": "command", "name": "tokenless-rewrite",
                    "command": "python3 /user/rewrite.py" }
                ] } ] },
              "plugins": { "enabled": ["tokenless@local"] }
            }"#,
        ));
        assert_eq!(
            settings_managed_present(&root, &managed_specs(), "tokenless@local"),
            (false, true),
            "a same-name hook with a different body must not read as present"
        );
    }

    #[test]
    fn settings_managed_present_fails_closed_without_bundle_spec() {
        let root = obj(json(
            r#"{ "hooks": { "PreToolUse": [
                { "hooks": [ { "name": "tokenless-rewrite" } ] } ] } }"#,
        ));
        let (hooks_present, _) = settings_managed_present(&root, &[], "tokenless@local");
        assert!(!hooks_present);
    }

    #[test]
    fn collect_expected_hook_names_reads_all_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("adapters").join("tokenless").join("qoder");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(
            root.join(QODER_HOOKS_FILE),
            br#"{"hooks":{
              "PreToolUse":[{"hooks":[{"name":"tokenless-rewrite"}]}],
              "PostToolUse":[{"hooks":[{"name":"tokenless-compress-response"}]}]
            }}"#,
        )
        .expect("write hooks");
        let mut names = collect_expected_hook_names(&root).expect("collect");
        names.sort();
        assert_eq!(
            names,
            vec!["tokenless-compress-response", "tokenless-rewrite"]
        );
    }

    #[test]
    fn load_settings_for_merge_fails_closed_on_unmergeable_json() {
        let path = Path::new("/home/u/.qoder/settings.json");
        assert!(
            load_settings_for_merge(None, path)
                .expect("absent ok")
                .is_empty()
        );
        assert!(
            matches!(
                load_settings_for_merge(Some(b"[1,2,3]".to_vec()), path),
                Err(AdapterError::SettingsUnparseable { .. })
            ),
            "non-object JSON must be left untouched by enable"
        );
        let err = load_settings_for_merge(Some(b"{not json".to_vec()), path)
            .expect_err("malformed must fail closed");
        assert!(matches!(err, AdapterError::SettingsUnparseable { .. }));
    }

    #[test]
    fn load_settings_for_merge_fails_closed_on_bad_nested_shapes() {
        let path = Path::new("/home/u/.qoder/settings.json");
        for bad in [
            br#"{ "hooks": "disabled" }"#.as_slice(),
            br#"{ "hooks": { "PreToolUse": "disabled" } }"#.as_slice(),
            br#"{ "plugins": "disabled" }"#.as_slice(),
            br#"{ "plugins": { "enabled": "tokenless@local" } }"#.as_slice(),
        ] {
            let err = load_settings_for_merge(Some(bad.to_vec()), path)
                .expect_err("bad shape must fail closed");
            assert!(
                matches!(err, AdapterError::SettingsUnparseable { .. }),
                "{err:?}"
            );
        }
    }

    #[test]
    fn merge_managed_rejects_bad_nested_shapes() {
        for mut root in [
            obj(json(r#"{ "hooks": "disabled" }"#)),
            obj(json(r#"{ "hooks": { "PreToolUse": "disabled" } }"#)),
            obj(json(r#"{ "plugins": "disabled" }"#)),
            obj(json(r#"{ "plugins": { "enabled": "tokenless@local" } }"#)),
        ] {
            assert!(
                merge_managed(&mut root, &managed_specs(), "tokenless@local").is_err(),
                "merge must not coerce malformed settings fields"
            );
        }
    }

    #[test]
    fn load_resolved_hooks_substitutes_placeholder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("adapters").join("tokenless").join("qoder");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(
            root.join(QODER_HOOKS_FILE),
            br#"{"hooks":{"PreToolUse":[{"hooks":[{"name":"tokenless-rewrite","command":"bash ${QODER_TOKENLESS_HOOKS}/rewrite.sh"}]}]}}"#,
        )
        .expect("write hooks");
        let resolved = load_resolved_hooks(&root).expect("resolve");
        let cmd = resolved["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command");
        assert!(
            cmd.contains("adapters/tokenless/common/hooks/rewrite.sh"),
            "{cmd}"
        );
        assert!(!cmd.contains(HOOKS_PLACEHOLDER));
    }
}
