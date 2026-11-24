"""Select additional component checks for edits to the shared tag workflow."""

import json
import os
import subprocess
from pathlib import Path

import yaml

RELEASE = ".github/workflows/release.yaml"
WORKFLOWS = Path(".github/workflows")


def shared_sections(source: str) -> tuple[str, str] | None:
    """Return the verbatim text outside the existing AW reusable-workflow call."""
    if any(isinstance(token, yaml.tokens.AliasToken) for token in yaml.scan(source)):
        # A value declared inside the AW job could be consumed by another job.
        return None
    root = yaml.compose(source, Loader=yaml.BaseLoader)

    def field(mapping, name):
        if not isinstance(mapping, yaml.MappingNode):
            return None
        nodes = [value for key, value in mapping.value if key.value == name]
        return nodes[0] if len(nodes) == 1 else None

    job = field(field(root, "jobs"), "aw_assets")
    for key, expected in (("uses", "./.github/workflows/aw-release.yml"),
                          ("if", "needs.package.outputs.component == 'aw'")):
        value = field(job, key)
        if not isinstance(value, yaml.ScalarNode) or value.value != expected:
            return None
    return source[:job.start_mark.index], source[job.end_mark.index:]


def matches(path: str, pattern: str) -> bool:
    # The component workflows use literal paths and directory prefixes.
    literal = pattern.removesuffix("/**")
    if not literal or any(c in literal for c in "*?!+[]\\"):
        raise ValueError(f"unsupported prebuilt trigger pattern: {pattern}")
    if pattern.endswith("/**"):
        return path.startswith(pattern[:-2])
    return path == pattern


def select(event: str, branch: str, changed: set[str] | None) -> list[str]:
    result = []
    for path in sorted(WORKFLOWS.glob("*-prebuilt-ci.yaml")):
        triggers = yaml.load(path.read_text(), Loader=yaml.BaseLoader)["on"]
        if event not in triggers:
            continue
        trigger = triggers[event]
        if "branches" in trigger and not any(matches(branch, p) for p in trigger["branches"]):
            continue
        # GitHub limits its path-filter comparison to 300 files. With a larger
        # or unavailable diff, keep coverage even if that means duplicate work.
        if changed is not None and len(changed) <= 300 and any(
            matches(name, pattern) for name in changed for pattern in trigger["paths"]
        ):
            continue
        result.append(path.name.removesuffix("-prebuilt-ci.yaml"))
    return result


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], timeout=30)


def shared_release_changed(base: str, head: str) -> bool:
    if not git("diff", "--name-only", base, head, "--", RELEASE):
        return False
    sections = []
    for revision in (base, head):
        blob = subprocess.run(["git", "show", f"{revision}:{RELEASE}"], capture_output=True, timeout=30)
        if blob.returncode:
            return True
        sections.append(shared_sections(blob.stdout.decode()))
    return sections[0] is None or sections[0] != sections[1]


def main() -> None:
    event = os.environ["GITHUB_EVENT_NAME"]
    payload = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_bytes())
    if event == "pull_request":
        pr = payload["pull_request"]
        head = pr["head"]["sha"]
        base = git("merge-base", pr["base"]["sha"], head).decode().strip()
        branch = pr["base"]["ref"]
    else:
        base, head = payload["before"], payload["after"]
        branch = os.environ["GITHUB_REF_NAME"]

    changed = None
    shared = True
    if base != "0" * 40 and subprocess.run(
        ["git", "cat-file", "-e", f"{base}^{{commit}}"], timeout=10,
    ).returncode == 0:
        changed = set(os.fsdecode(git("diff", "--name-only", "-z", "--find-renames", base, head)).split("\0"))
        changed.discard("")
        shared = shared_release_changed(base, head)
    components = select(event, branch, changed) if shared else []
    print("Additional prebuilt checks:", ", ".join(components) or "none")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        print("components=" + json.dumps(components), file=output)


if __name__ == "__main__":
    main()
