"""Find previously accepted AW assets for an official release tag."""

import json
import os
import re
import subprocess
from pathlib import Path

REPOSITORY = "agentic-os-org/ANOLISA"
WORKFLOW = "aw-release.yml"


def api(path: str) -> dict:
    data = subprocess.check_output(
        ["gh", "api", f"repos/{REPOSITORY}/actions/{path}"], text=True, timeout=20
    )
    return json.loads(data)


def accepted_artifact(sha: str, repository: str) -> dict | None:
    """Pin promotion to the latest matching dispatch and its successful attempt."""
    if repository != REPOSITORY or not re.fullmatch(r"[0-9a-f]{40}", sha):
        return None
    runs = api(
        f"workflows/{WORKFLOW}/runs?branch=main&event=workflow_dispatch&head_sha={sha}&per_page=1"
    )["workflow_runs"]
    if not runs:
        return None
    run = runs[0]
    if not (
        run["event"] == "workflow_dispatch" and run["head_branch"] == "main"
        and run["path"] == f".github/workflows/{WORKFLOW}" and run["head_sha"] == sha
        and run["head_repository"]["full_name"] == repository
        and run["status"] == "completed" and run["conclusion"] == "success"
    ):
        return None

    name = f"aw-release-{sha}-{run['run_attempt']}"
    artifacts = api(f"runs/{run['id']}/artifacts?name={name}")["artifacts"]
    if len(artifacts) == 1:
        artifact = artifacts[0]
        source = artifact.get("workflow_run") or {}
        if (
            source.get("id") == run["id"] and source.get("head_sha") == sha
            and artifact["name"] == name and not artifact["expired"]
            and re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest") or "")
        ):
            return artifact
    return None


def main() -> None:
    try:
        artifact = accepted_artifact(os.environ["GITHUB_SHA"], os.environ["GITHUB_REPOSITORY"])
    except subprocess.SubprocessError as error:
        print(f"::warning::Candidate lookup unavailable; running release validation ({error})")
        return
    if artifact is None:
        print("No accepted candidate artifact; running release validation")
        return
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as stream:
        stream.write(f"artifact_id={artifact['id']}\ndigest={artifact['digest']}\n")
    run_id = artifact["workflow_run"]["id"]
    print(f"Promoting https://github.com/{REPOSITORY}/actions/runs/{run_id}/artifacts/{artifact['id']}")


if __name__ == "__main__":
    main()
