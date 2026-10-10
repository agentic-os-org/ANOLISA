# Offline configuration editing

[中文版](configuration-editing_zh.md)

`aw_package::config_edit::ConfigurationEditor` edits an existing desired YAML or
JSON configuration. The packaging CLI exposes the same boundary through
`aw-package config`. It does not install an Agent, run a command, discover Provider
capabilities or establish native effect support.

## Editing contract

`open` validates the complete existing file using the bundled `aw-config`
validator. `as_value`, `provider` and `hook` expose the working document, a named
Provider and an event-scoped step respectively. These values can contain private
Provider settings; callers decide what may be displayed.

`add_provider` inserts an explicit complete Provider definition. `add_hook`
appends a complete structured or native step to an existing event. To use an
absent event, call `add_event` first with its explicit event options and steps;
the editor never enables an event implicitly. Existing step order, event options
and unrelated configuration values are preserved. Optional fields remain omitted
and unknown Provider-owned `config` keys remain intact. Unknown public fields,
duplicate input keys and mismatched Provider/step forms are rejected by the
existing schema and static validator.

Add operations are idempotent when the same name or event-scoped step ID has an
identical value. A different value under that identity is a conflict, rather than
an overwrite. Remove operations are idempotent when the identity is absent.
`remove_hook` retains the event and its options when its last step is removed.
`remove_provider` rejects outstanding references, including disabled steps; it
never removes another Hook implicitly.

Every mutation validates a candidate before replacing the working document.
Invalid input and conflicts preserve both the working document and the original
file. Mutations are staged in memory until `save`; `validate` is an offline check.

```rust
use aw_package::config_edit::ConfigurationEditor;
use serde_json::json;
use std::path::Path;

let mut editor = ConfigurationEditor::open(Path::new("/home/user/aw.yaml"))?;
editor.add_provider("native", json!({
    "protocol": "native-hook/v1alpha1",
    "transport": {
        "type": "stdio", "location": "agent", "argv": ["/opt/company/hook"]
    },
    "timeout_ms": 1000, "max_output_bytes": 4096, "config": {}
}))?;
// Use add_event only if the event is absent; preserve an existing event's options.
if editor.as_value()["spec"]["events"].get("tool.before").is_none() {
    editor.add_event("tool.before", json!({"enabled": true, "steps": []}))?;
}
editor.add_hook("tool.before", json!({
    "id": "company-before", "provider": "native", "native": {},
    "on_error": "report"
}))?;
editor.validate()?;
editor.save()?;
```

## Publication boundary

The file must be an owned regular file with no hard links, special mode bits or
group/world write permissions. Its absolute path must contain no symlinks or
parent traversal, and its parent must be owned and not group/world writable.
Input, expanded data and depth remain bounded by the existing validator.

`save` takes a nonblocking exclusive lock on the current file, compares its
device/inode, mode, size, modification time and exact bytes with the original
snapshot, and repeats that check immediately before publication. Concurrent
editors either encounter contention or reject a stale snapshot, including when
another editor has atomically replaced the original inode. Reopen to apply edits
to a newer document; the editor does not retry or merge changes automatically.
Other writers must honor the file lock to avoid the final check/rename race.

A semantic no-op, including edits later reversed, preserves original bytes and
inode. Changed documents are serialized to YAML in a same-directory temporary
file, retain permission bits, and are synced before atomic replacement. Failed
validation, contention, snapshot checks or staging writes leave the original
file intact and remove owned temporary files. The commit point has no subsequent
fallible operation. Comments, formatting and key order are rewritten on a real
change; extended attributes and ACLs are not preserved. This is atomic
publication, not a power-loss durability guarantee. Configuration byte changes
also change the runtime configuration revision.

## CLI boundary

`aw-package config` exposes `show`, `validate`, `add-provider`, `remove-provider`,
`add-event`, `add-hook` and `remove-hook`. Each mutating invocation stages one
operation and saves once; separate invocations are not a batch transaction.
Library callers may stage several validated operations before saving. Required,
unknown, duplicate and conflicting selector options are checked before opening
configuration or definition files.

Add operations accept a complete YAML/JSON object through `--definition FILE`.
The file may use a relative path but must be regular; the bounded read and
`aw_config::parse_value` reuse the existing 4 MiB/depth-32 parser, preserving its
duplicate-key, alias-expansion and JSON-compatible type checks before the editor
validates the resulting complete configuration. There is no stdin input or
command execution. `show` prints pretty JSON, including private Provider values;
selectors choose a Provider, event or event-scoped step, and absent selections
are errors. `show` and `validate` publish nothing.

Mutations print `Updated configuration` or `Configuration unchanged`;
`validate` prints `Valid configuration`. CLI editing changes no Schema,
registry or service contracts and does not reload the running service. See the
[CLI reference](../../../../docs/user-guide/en/user-entrypoint/aw-preview.md#edit-existing-provider-and-hook-configuration)
for options, rollback and restarting with the edited revision.
