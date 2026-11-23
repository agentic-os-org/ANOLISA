"""Bind shipped files and verified build-input inventories to the release archive."""

import json
from pathlib import Path

from package import digest


def namespace(value: object, prefix: str) -> object:
    """Keep references from independent upstream SBOMs from colliding."""
    if isinstance(value, list):
        return [namespace(item, prefix) for item in value]
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            if key in {"bom-ref", "ref"} and isinstance(item, str):
                result[key] = prefix + item
            elif key == "dependsOn":
                result[key] = [prefix + name for name in item]
            else:
                result[key] = namespace(item, prefix)
        return result
    return value


def cargo_inventory(document: dict, package_name: str) -> tuple[list[dict], list[dict], str]:
    """Describe the selected program's resolved normal and build dependencies."""
    packages = {item["id"]: item for item in document["packages"]}
    nodes = {item["id"]: item for item in document["resolve"]["nodes"]}
    root = next(item["id"] for item in packages.values() if item["name"] == package_name)
    seen = set()
    todo = [root]
    edges = {}
    while todo:
        item = todo.pop()
        if item in seen:
            continue
        seen.add(item)
        dependencies = {
            dep["pkg"]
            for dep in nodes[item]["deps"]
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
        }
        edges[item] = dependencies
        todo.extend(dependencies - seen)
    references = {
        item: f"cargo:{package_name}:{packages[item]['name']}@{packages[item]['version']}"
        for item in seen
    }
    components = []
    for item in sorted(seen):
        package = packages[item]
        component = {
            "type": "library",
            "name": package["name"],
            "version": package["version"],
            "bom-ref": references[item],
        }
        if package.get("license"):
            component["licenses"] = [{"expression": package["license"]}]
        components.append(component)
    dependencies = [
        {"ref": references[item], "dependsOn": sorted(references[dep] for dep in edges[item])}
        for item in sorted(seen)
    ]
    return components, dependencies, references[root]


def write_sbom(archive: Path, manifest: dict, upstream: dict, cargo: dict) -> Path:
    """Combine input inventories and identify every installed file by hash.

    Upstream inventories describe verified build inputs, not extra installed
    adapters. The shipped-file entries and bundle manifest define the payload.
    """
    version = manifest["version"]
    identity = f"omarchy-{version}-linux-x86_64-tar"
    components = []
    dependencies = []
    roots = []
    for name, document in sorted(upstream.items()):
        prefixed = namespace(document, f"upstream:{name}:")
        component = prefixed["metadata"]["component"]
        component.setdefault("properties", []).append(
            {"name": "anolisa:inventory:role", "value": "verified-build-input"}
        )
        components.extend([component, *prefixed.get("components", [])])
        dependencies.extend(prefixed.get("dependencies", []))
        roots.append(component["bom-ref"])
    for name, document in sorted(cargo.items()):
        children, edges, root = cargo_inventory(document, name)
        components.extend(children)
        dependencies.extend(edges)
        roots.append(root)
    shipped = {"payload/" + name: checksum for name, checksum in manifest["files"].items()}
    shipped.update(manifest["install_files"])
    for name, checksum in sorted(shipped.items()):
        reference = "shipped:" + name
        components.append(
            {
                "type": "file",
                "name": name,
                "bom-ref": reference,
                "hashes": [{"alg": "SHA-256", "content": checksum}],
            }
        )
        roots.append(reference)
    dependencies.append({"ref": identity, "dependsOn": sorted(roots)})
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "tools": [{"name": "anolisa-omarchy-packager", "version": "1"}],
            "component": {
                "type": "file",
                "name": "omarchy",
                "version": version,
                "bom-ref": identity,
                "hashes": [{"alg": "SHA-256", "content": digest(archive)}],
                "licenses": [{"license": {"id": "Apache-2.0"}}],
                "properties": [
                    {"name": "anolisa:artifact:name", "value": archive.name},
                    {"name": "anolisa:target:os", "value": "linux"},
                    {"name": "anolisa:target:arch", "value": "x86_64"},
                    {"name": "anolisa:source:commit", "value": manifest["source_commit"]},
                    {
                        "name": "anolisa:source:tree-sha256",
                        "value": manifest["source"]["tree_sha256"],
                    },
                ],
            },
        },
        "components": components,
        "dependencies": dependencies,
    }
    target = Path(str(archive) + ".cdx.json")
    target.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
    )
    return target
