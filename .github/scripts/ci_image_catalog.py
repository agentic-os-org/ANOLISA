"""Validate the CI image catalog and print the build matrix for this change."""
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(".github/runner-images")
CATALOG = ROOT / "catalog.json"
WORKFLOW = ".github/workflows/sec-core-ci-image.yaml"
SLUG = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")


def load_images():
    images = json.loads(CATALOG.read_text())
    if not isinstance(images, list):
        raise ValueError("Expected a list of CI images")
    aliases = set()
    for item in images:
        if not isinstance(item, dict) or item.keys() != {"alias", "context"}:
            raise ValueError("Each image needs an alias and a context")
        alias, context = item["alias"], item["context"]
        if not all(isinstance(v, str) and SLUG.fullmatch(v) for v in (alias, context)):
            raise ValueError("Use lowercase letters, digits and single hyphens")
        if not 2 <= len(alias) <= 63 or alias in aliases:
            raise ValueError(f"Invalid or duplicate CI image alias: {alias}")
        aliases.add(alias)
        directory = ROOT / context
        recipe = directory / "Dockerfile"
        if directory.is_symlink() or recipe.is_symlink() or not recipe.is_file():
            raise ValueError(f"Expected a Dockerfile inside {directory}")
    return images


def selected(images, base):
    if not re.fullmatch(r"[0-9a-f]{40}", base):
        return images
    diff = subprocess.run(["git", "diff", "--no-renames", "--name-only", "-z", base, "HEAD"],
                          stdout=subprocess.PIPE, check=False)
    if diff.returncode:
        return images
    changed = set(os.fsdecode(diff.stdout).split("\0"))
    if {WORKFLOW, ".github/scripts/ci_image_catalog.py"} & changed:
        return images
    old = subprocess.run(["git", "show", f"{base}:{CATALOG}"], capture_output=True)
    if old.returncode:
        return images
    previous = json.loads(old.stdout)
    return [img for img in images if img not in previous or
            any(p.startswith(f"{ROOT}/{img['context']}/") for p in changed)]


if __name__ == "__main__":
    print(json.dumps(selected(load_images(), os.environ.get("BASE_REV", "")),
                     separators=(",", ":")))
