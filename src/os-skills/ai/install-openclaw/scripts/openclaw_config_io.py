"""Configuration merge and JSON persistence policy for the OpenClaw installer."""

import json
import os


def deep_merge(base, override):
    result = dict(base)
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result


def ordered_unique(items):
    result = []
    for item in items:
        if item and item not in result:
            result.append(item)
    return result


def merge_plugin_allow(existing, merged):
    existing_allow = existing.get("plugins", {}).get("allow", [])
    merged_allow = merged.get("plugins", {}).get("allow")
    if merged_allow is None:
        return merged

    merged.setdefault("plugins", {})["allow"] = ordered_unique([*existing_allow, *merged_allow])
    return merged


def apply_config(config, config_path, *, dry_run=False):
    print("\n--- Writing OpenClaw config ---\n")
    if dry_run:
        print(f"  # dry-run: would write OpenClaw config to {config_path}")
        for key in config:
            print(f"  [OK] {key}")
        return

    existing = {}
    if config_path.exists():
        try:
            with config_path.open("r", encoding="utf-8") as fh:
                existing = json.load(fh)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"Invalid JSON in {config_path}: {exc}") from exc

    merged = deep_merge(existing, config)
    merged = merge_plugin_allow(existing, merged)

    config_path.parent.mkdir(parents=True, exist_ok=True)
    if config_path.exists():
        backup_path = config_path.with_name(config_path.name + ".bak")
        backup_path.write_bytes(config_path.read_bytes())

    tmp_path = config_path.with_name(config_path.name + ".tmp")
    with tmp_path.open("w", encoding="utf-8") as fh:
        json.dump(merged, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    os.replace(tmp_path, config_path)

    for key in config:
        print(f"  [OK] {key}")

    print(f"\nConfig written: {config_path}")
