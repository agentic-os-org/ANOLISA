"""Explicit maintainer build: image, inventory and offline Docker archive."""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

DEFAULT_BASE_IMAGE = (
    "ghcr.io/protectskills/claude-skill-sandbox:lite@sha256:"
    "f6f993df6f9c3f8b608c50548da598f4be862e020f204afa3d653701e95e07bc"
)
IMAGE_ID = r"sha256:[a-f0-9]{64}"
REGISTRY_DIGEST = r"[a-zA-Z0-9._:/-]+@" + IMAGE_ID


def inspect_base(reference: str) -> dict:
    """Resolve an immutable reference without substituting a mutable tag."""
    local = re.fullmatch(IMAGE_ID, reference)
    if not local and not re.fullmatch(REGISTRY_DIGEST, reference):
        raise ValueError(
            "base image must be a registry digest or local SHA256 image ID"
        )
    inspected = subprocess.run(
        ["docker", "image", "inspect", reference],
        check=False,
        capture_output=True,
        text=True,
    )
    if inspected.returncode and not local:
        subprocess.run(
            ["docker", "pull", "--platform", "linux/amd64", reference], check=True
        )
        inspected = subprocess.run(
            ["docker", "image", "inspect", reference],
            check=True,
            capture_output=True,
            text=True,
        )
    inspected.check_returncode()
    details = json.loads(inspected.stdout)
    if len(details) != 1:
        raise ValueError("base image inspection must identify exactly one image")
    detail = details[0]
    identity = detail.get("Id", "")
    if (
        not re.fullmatch(IMAGE_ID, identity)
        or (detail.get("Os"), detail.get("Architecture")) != ("linux", "amd64")
        or (local and identity != reference)
    ):
        raise ValueError("base image must resolve to its exact linux/amd64 image ID")
    if not local:
        repository, digest = reference.rsplit("@", 1)
        canonical = re.sub(r":[^/:]+$", "", repository) + "@" + digest
        if not {reference, canonical}.intersection(detail.get("RepoDigests", [])):
            raise ValueError(
                "base image inspection did not confirm its registry digest"
            )
    return {"reference": reference, "image_id": identity, "platform": "linux/amd64"}


def work_base_image(reference: str, recipe_path: Path | None = None) -> dict:
    """Bind a prepared business image to its actual identity and source recipe."""
    base = inspect_base(reference)
    if recipe_path is None:
        if re.fullmatch(IMAGE_ID, reference):
            raise ValueError("a local base image requires --base-image-recipe")
        return base
    recipe = json.loads(recipe_path.read_text())
    if (
        not isinstance(recipe, dict)
        or set(recipe)
        != {
            "source_repository",
            "source_commit",
            "dockerfile_path",
            "dockerfile_sha256",
            "upstream_base_image",
        }
        or not re.fullmatch(
            r"https://[a-zA-Z0-9._/-]+", recipe.get("source_repository", "")
        )
        or not re.fullmatch(r"[a-f0-9]{40}", recipe.get("source_commit", ""))
        or not re.fullmatch(r"[a-f0-9]{64}", recipe.get("dockerfile_sha256", ""))
        or not recipe.get("dockerfile_path")
        or Path(recipe["dockerfile_path"]).is_absolute()
        or ".." in Path(recipe["dockerfile_path"]).parts
    ):
        raise ValueError(
            "base image recipe requires a fixed repository, commit and Dockerfile digest"
        )
    upstream = recipe["upstream_base_image"]
    if (
        not isinstance(upstream, dict)
        or set(upstream) != {"reference", "image_id", "platform"}
        or not re.fullmatch(REGISTRY_DIGEST, upstream.get("reference", ""))
        or inspect_base(upstream["reference"]) != upstream
    ):
        raise ValueError(
            "base image recipe upstream identity differs from Docker inspection"
        )
    base["recipe"] = recipe
    return base


def validate_base_version(version: str, base: dict) -> None:
    """Keep custom-base artifacts separate from the automatic default release."""
    suffix = ".base." + base["image_id"].removeprefix("sha256:")[:12]
    if base["reference"] != DEFAULT_BASE_IMAGE:
        if not version.endswith(suffix):
            raise ValueError(f"custom base version must end with {suffix}")
    elif ".base." in version:
        raise ValueError("default base version must not include a custom-base suffix")


@contextmanager
def build_base_reference(base: dict) -> Iterator[str]:
    """Give BuildKit a local name without misrepresenting an image ID as a digest."""
    if "@sha256:" in base["reference"]:
        yield base["reference"]
        return
    tag = "agent-sec-skill-score-base:" + base["image_id"].replace(":", "-")
    existing = subprocess.run(
        ["docker", "image", "inspect", tag],
        check=False,
        capture_output=True,
        text=True,
    )
    created = existing.returncode != 0
    if created:
        subprocess.run(["docker", "tag", base["image_id"], tag], check=True)

    def verify() -> dict:
        details = json.loads(
            subprocess.run(
                ["docker", "image", "inspect", tag],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )
        if len(details) != 1 or (
            details[0].get("Id"),
            details[0].get("Os"),
            details[0].get("Architecture"),
        ) != (base["image_id"], "linux", "amd64"):
            raise ValueError(
                "local build base tag differs from its immutable image identity"
            )
        return details[0]

    verify()
    try:
        yield tag
    finally:
        # Do not remove an existing tag or a tag changed by another build.
        detail = verify()
        if created:
            if any(name != tag for name in detail.get("RepoTags") or []):
                subprocess.run(["docker", "image", "rm", tag], check=True)
            else:
                # Removing an image's last tag also removes its digest references.
                print(
                    f"Retaining {tag}: removing its only tag could delete the base image",
                    file=sys.stderr,
                )


def archive_image_identities(archive_path: Path, names: tuple[str, ...]) -> list[dict]:
    """Bind classic config IDs and containerd manifest IDs to the same saved images."""
    with tarfile.open(archive_path) as archive:
        members = {member.name: member for member in archive.getmembers()}
        if len(members) != len(archive.getmembers()):
            raise ValueError("image archive contains duplicate members")

        def read_json(name: str, descriptor: dict | None = None) -> dict | list:
            member = members.get(name)
            if member is None or not member.isfile() or member.size > 1024 * 1024:
                raise ValueError("image archive metadata is missing or invalid")
            with archive.extractfile(member) as stream:
                data = stream.read()
            if descriptor is not None and (
                descriptor.get("size") != len(data)
                or descriptor.get("digest")
                != "sha256:" + hashlib.sha256(data).hexdigest()
            ):
                raise ValueError("image archive descriptor checksum/size mismatch")
            return json.loads(data)

        def blob(descriptor: dict) -> str:
            identity = descriptor.get("digest", "")
            if not re.fullmatch(r"sha256:[a-f0-9]{64}", identity):
                raise ValueError("image archive descriptor must use SHA256")
            return "blobs/sha256/" + identity.removeprefix("sha256:")

        index = read_json("index.json")
        legacy = read_json("manifest.json")
        if (
            index.get("schemaVersion") != 2
            or len(index.get("manifests", [])) != len(names)
            or len(legacy) != len(names)
        ):
            raise ValueError("image archive must contain exactly the release images")
        records = {}
        for descriptor in index["manifests"]:
            annotations = descriptor.get("annotations", {})
            name = annotations.get("io.containerd.image.name", "")
            name = name.removeprefix("docker.io/library/")
            if name not in names or name in records:
                raise ValueError("image archive tag is unexpected or ambiguous")
            if descriptor.get("mediaType") not in {
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.docker.distribution.manifest.v2+json",
            }:
                raise ValueError(
                    "image archive must bind a single-platform image manifest"
                )
            manifest = read_json(blob(descriptor), descriptor)
            if manifest.get("schemaVersion") != 2:
                raise ValueError("image archive manifest schema is unsupported")
            config = read_json(blob(manifest["config"]), manifest["config"])
            if (config.get("os"), config.get("architecture")) != ("linux", "amd64"):
                raise ValueError("image archive platform must be linux/amd64")
            layers = [blob(layer) for layer in manifest["layers"]]
            matches = [item for item in legacy if item.get("RepoTags") == [name]]
            if (
                len(matches) != 1
                or matches[0].get("Config") != blob(manifest["config"])
                or matches[0].get("Layers") != layers
            ):
                raise ValueError("classic and OCI archive image identities disagree")
            for layer, path in zip(manifest["layers"], layers, strict=True):
                member = members.get(path)
                if (
                    member is None
                    or not member.isfile()
                    or member.size != layer.get("size")
                ):
                    raise ValueError(
                        "image archive layer is missing or has a different size"
                    )
            records[name] = {
                "name": name,
                "id": manifest["config"]["digest"],
                "manifest_id": descriptor["digest"],
                "platform": "linux/amd64",
            }
        return [records[name] for name in names]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument(
        "--build-network", choices=("default", "host"), default="default"
    )
    parser.add_argument(
        "--version", required=True, help="Component version used for both image tags"
    )
    parser.add_argument("--base-image", default=DEFAULT_BASE_IMAGE)
    parser.add_argument(
        "--base-image-recipe",
        type=Path,
        help="JSON source recipe required for a locally prepared business image",
    )
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}", args.version):
        parser.error("version is not a Docker tag")
    base = work_base_image(args.base_image, args.base_image_recipe)
    validate_base_version(args.version, base)
    component = Path(__file__).resolve().parents[2]
    image = f"agent-sec-skill-score:{args.version}"
    args.output.mkdir(parents=True, exist_ok=True)
    with (
        tempfile.TemporaryDirectory(prefix="skill-score-image-") as temporary,
        build_base_reference(base) as base_reference,
    ):
        context = Path(temporary)
        for name in ("Dockerfile", "observe.py", "claude-observed"):
            shutil.copy2(Path(__file__).parent / name, context / name)
        source = component / "agent-sec-cli/src/agent_sec_cli"
        target = context / "scanner-source/agent_sec_cli"
        target.mkdir(parents=True)
        shutil.copy2(source / "__init__.py", target / "__init__.py")
        # Only genuine code-scanner modules needed by the subject scanner Skill, not CLI/ML.
        for name in ("code_scanner", "model_service"):
            shutil.copytree(
                source / name,
                target / name,
                ignore=shutil.ignore_patterns("__pycache__"),
            )
        subprocess.run(
            [
                "docker",
                "build",
                "--provenance=false",
                "--network",
                args.build_network,
                "--pull=false",
                "--platform",
                "linux/amd64",
                "--build-arg",
                f"BASE_IMAGE={base_reference}",
                "-t",
                image,
                str(context),
            ],
            check=True,
        )
    sidecar = f"agent-sec-skill-score-egress:{args.version}"
    subprocess.run(
        [
            "docker",
            "build",
            "--provenance=false",
            "--network",
            args.build_network,
            "--pull=false",
            "--platform",
            "linux/amd64",
            "-t",
            sidecar,
            str(component / "skill-score-runtime/egress"),
        ],
        check=True,
    )
    output = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "python",
            image,
            "/opt/poc/observe.py",
            "inventory",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    inventory = json.loads(output.stdout)
    inventory.update(
        docker_image=image,
        work_base_image=base,
        network="ASC static model-host allowlist for proxied TCP; local destinations and ICMP remain direct",
        mock="http://127.0.0.1:8765; static routes, no downstream model inference",
    )
    text = json.dumps(inventory, ensure_ascii=False, indent=2) + "\n"
    (args.output / "tool-inventory.json").write_text(text)
    subprocess.run(
        [
            "docker",
            "save",
            "-o",
            str(args.output / "skill-score-images.tar"),
            image,
            sidecar,
        ],
        check=True,
    )
    details = json.loads(
        subprocess.run(
            ["docker", "image", "inspect", image, sidecar],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    identities = archive_image_identities(
        args.output / "skill-score-images.tar", (image, sidecar)
    )
    for identity, detail in zip(identities, details, strict=True):
        if detail["Id"] not in (identity["id"], identity["manifest_id"]) or (
            detail["Os"],
            detail["Architecture"],
        ) != ("linux", "amd64"):
            raise ValueError("built image differs from its saved immutable identities")
        identity["repo_digests"] = detail.get("RepoDigests", [])
    claude = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "/opt/asc/claude/node_modules/.bin/claude",
            image,
            "--version",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    if claude.stdout.strip() != "2.1.143 (Claude Code)":
        raise ValueError("work image Claude version differs from ScoreConfig 2.1.143")
    gost = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "/bin/gost",
            sidecar,
            "-V",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    (args.output / "images.json").write_text(
        json.dumps(
            {
                "images": [image, sidecar],
                "image_details": identities,
                "work_base_image": base,
                "versions": {
                    "harbor": "0.22.0",
                    "claude": claude.stdout.strip(),
                    "gost": gost.stdout.strip(),
                    "observer_sha256": hashlib.sha256(
                        (Path(__file__).parent / "observe.py").read_bytes()
                    ).hexdigest(),
                },
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
