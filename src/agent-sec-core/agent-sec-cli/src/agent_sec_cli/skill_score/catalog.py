"""Keep a bounded Judge view while preserving raw Runtime evidence."""

import json
import re


def judge_catalog(
    catalog: dict[str, str], required_refs: set[str] | None = None
) -> tuple[dict[str, str], dict[str, int]]:
    """Collapse repeated syscall facts, retaining the full catalog on disk."""
    result, groups = {}, {}
    for ref, text in catalog.items():
        if not ref.startswith("event:"):
            result[ref] = text
            continue
        event = json.loads(text)
        view = dict(event)
        if event.get("matched"):
            result[ref] = text
            continue
        if event.get("kind") == "file":
            if not event.get("write") and (
                event.get("syscall") not in {"open", "openat", "openat2"}
                or "O_DIRECTORY" in event.get("args", "")
                or any(code in event.get("result", "") for code in ("ENOENT", "ENOTDIR"))
            ):
                # Metadata lookups stay in raw evidence; expose meaningful reads and writes.
                if ref not in (required_refs or set()):
                    continue
            # Path, operation, write flag and syscall result express the file fact.
            view.pop("args", None)
            if event.get("outcome") == "succeeded":
                view["result"] = "succeeded"  # file descriptor numbers are not distinct effects
        elif event.get("kind") == "network":
            view.pop("target", None)  # target duplicates args verbatim for network events
            if event.get("syscall") != "connect":
                args = view.pop("args", "")
                view["socket"] = args.split(",", 1)[0]
                address = re.search(r"\{sa_family=.*?\}", args)
                view["destination"] = address[0] if address else "see socket peer"
                view["payload"] = "retained in raw event and scanner input, omitted from Judge view"
        identity = {
            key: value for key, value in view.items() if key not in {"time", "pid", "ppid", "id"}
        }
        signature = re.sub(r"0x[0-9a-fA-F]+", "<address>", json.dumps(identity, sort_keys=True))
        if signature in groups:
            groups[signature][1] += 1
            if ref in (required_refs or set()):
                result[ref] = json.dumps(view, ensure_ascii=False)
        else:
            groups[signature] = [ref, 1, view]
    for ref, count, view in groups.values():
        view["equivalent_event_count"] = count
        result[ref] = json.dumps(view, ensure_ascii=False)
    return result, {
        "raw_references": len(catalog),
        "judge_references": len(result),
        "raw_characters": sum(map(len, catalog.values())),
        "judge_characters": sum(map(len, result.values())),
    }
