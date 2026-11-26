#!/usr/bin/env python3
"""Retry only scheduling preemption of AgentSight resource jobs."""

from __future__ import annotations

import json
import os
import re
import time
import urllib.request
from pathlib import Path
from typing import Any, Callable

JOB_NAME = "Test agentsight resources"
BACKOFF_SECONDS = (60, 300, 900)
RUN_ID_PATTERN = re.compile(r"[1-9][0-9]*")
JsonObject = dict[str, Any]
Sleep = Callable[[float], None]
Summary = Callable[[str], None]


class GitHubClient:
    """Small GitHub Actions API client that never logs credentials."""

    def __init__(self, token: str, repository: str, timeout: float = 30.0) -> None:
        self.repository = repository
        self.timeout = timeout
        self.headers = {
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "anolisa-resource-scheduler",
        }

    def get(self, path: str, repository: str | None = None) -> Any:
        with self._request("GET", path, repository=repository) as response:
            payload = response.read()
        return json.loads(payload) if payload else {}

    def post(self, path: str, payload: JsonObject | None = None) -> int:
        with self._request("POST", path, payload) as response:
            return response.status

    def patch(self, path: str, payload: JsonObject) -> int:
        with self._request("PATCH", path, payload) as response:
            return response.status

    def _request(
        self,
        method: str,
        path: str,
        payload: JsonObject | None = None,
        repository: str | None = None,
    ) -> Any:
        data = b"" if method == "POST" and payload is None else payload
        if data is not None and not isinstance(data, bytes):
            data = json.dumps(data).encode()
        headers = self.headers
        if data is not None:
            headers = {**headers, "Content-Type": "application/json"}
        request = urllib.request.Request(
            f"https://api.github.com/repos/{repository or self.repository}{path}",
            data=data,
            method=method,
            headers=headers,
        )
        return urllib.request.urlopen(request, timeout=self.timeout)


def default_summary(message: str) -> None:
    print(message, flush=True)
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with Path(summary_path).open("a", encoding="utf-8") as summary:
            summary.write(message + "\n")


class ResourceRetry:
    """Requeue only a still-current PR job whose cancellation was scheduler preemption."""

    def __init__(
        self,
        client: GitHubClient,
        run_id: str,
        sleep: Sleep = time.sleep,
        summarize: Summary = default_summary,
    ) -> None:
        if not RUN_ID_PATTERN.fullmatch(run_id):
            raise ValueError(f"invalid workflow run id: {run_id!r}")
        self.client = client
        self.run_id = run_id
        self.sleep = sleep
        self.summarize = summarize

    def run(self) -> int:
        run = self.client.get(f"/actions/runs/{self.run_id}")
        if run.get("event") != "pull_request":
            self.summarize(
                "No automatic retry: this is not a pull_request resource run."
            )
            return 0
        jobs = self.client.get(
            f"/actions/runs/{self.run_id}/attempts/{run['run_attempt']}/jobs?per_page=100"
        )["jobs"]
        job = next(
            (candidate for candidate in jobs if candidate["name"] == JOB_NAME), None
        )
        head_repository = (run.get("head_repository") or {}).get(
            "full_name", self.client.repository
        )
        if job is not None and job["conclusion"] == "success":
            self._resolve_action_required(job, head_repository)
            return 0
        if job is None or job["conclusion"] != "cancelled":
            self.summarize(
                "No automatic retry: the resource job was not cancelled by scheduling."
            )
            return 0
        annotations = self.client.get(f"/check-runs/{job['id']}/annotations")
        expected = (
            "Canceling since a higher priority waiting request for "
            f"{self.client.repository}-agentsight-resource-kernel66 exists"
        )
        preempted = any(item.get("message") == expected for item in annotations)
        if not preempted:
            self.summarize("No automatic retry: cancellation was not queue preemption.")
            return 0
        if job.get("steps") is None:
            self.summarize("No automatic retry: job execution state is unavailable.")
            return 0
        measurement_incomplete = any(
            step["name"] == "Measure resource usage"
            and step["conclusion"] in ("cancelled", "skipped")
            for step in job["steps"]
        )
        for step in job["steps"]:
            if step["conclusion"] not in ("failure", "timed_out"):
                continue
            if (
                step["conclusion"] == "failure"
                and step["name"] == "Enforce resource limits and publish summary"
                and measurement_incomplete
            ):
                continue
            self.summarize("No automatic retry: a test or validation step failed.")
            return 0

        # Manual reruns share the same attempt budget, preventing unbounded retry loops.
        attempt = int(job["run_attempt"])
        if not self._is_current_attempt(job):
            return 0
        if not self._has_open_pr(job["head_sha"], head_repository):
            self.summarize("Retry skipped: the PR is closed or its head changed.")
            return 0
        if attempt > len(BACKOFF_SECONDS):
            self._mark_action_required(job)
            return 0

        seconds = BACKOFF_SECONDS[attempt - 1]
        self.summarize(
            f"Retry {attempt}/3 scheduled in {seconds} seconds. "
            f"{self._preemption_summary(job)}"
        )
        self.sleep(seconds)
        if not self._is_current_attempt(job):
            return 0
        if not self._has_open_pr(job["head_sha"], head_repository):
            self.summarize(
                "Retry skipped after backoff: the PR is closed or its head changed."
            )
            return 0
        self.client.post(f"/actions/jobs/{job['id']}/rerun")
        self.summarize(
            f"Retry {attempt}/3 requeued run {self.run_id}. "
            f"{self._preemption_summary(job)}"
        )
        return 0

    def _is_current_attempt(self, job: JsonObject) -> bool:
        latest = self._latest_resource_check(job["head_sha"])
        if latest is None:
            self.summarize("Retry skipped: the current resource check cannot be found.")
            return False
        if latest["id"] == job["id"]:
            return True
        self.summarize("Retry skipped: a newer resource attempt already exists.")
        return False

    def _latest_resource_check(self, head_sha: str) -> JsonObject | None:
        checks = self.client.get(f"/commits/{head_sha}/check-runs?per_page=100")[
            "check_runs"
        ]
        candidates = [check for check in checks if check["name"] == JOB_NAME]
        return max(candidates, key=lambda check: check["id"]) if candidates else None

    def _preemption_summary(self, job: JsonObject) -> str:
        if job["steps"]:
            return "The resource job was preempted before producing a test verdict."
        return "The pending job was preempted before any test steps ran."

    def _latest_scheduling_check(self, head_sha: str) -> JsonObject | None:
        checks = self.client.get(f"/commits/{head_sha}/check-runs?per_page=100")[
            "check_runs"
        ]
        candidates = [
            check
            for check in checks
            if check["name"] == "AgentSight resource scheduling"
        ]
        return max(candidates, key=lambda check: check["id"]) if candidates else None

    def _create_scheduling_check(
        self, head_sha: str, conclusion: str, summary: str
    ) -> None:
        self.client.post(
            "/check-runs",
            {
                "name": "AgentSight resource scheduling",
                "head_sha": head_sha,
                "status": "completed",
                "conclusion": conclusion,
                "output": {
                    "title": "AgentSight resource scheduling",
                    "summary": summary,
                },
            },
        )

    def _mark_action_required(self, job: JsonObject) -> None:
        existing = self._latest_scheduling_check(job["head_sha"])
        if existing is not None and existing["conclusion"] == "action_required":
            self.summarize("Action required is already present on this PR head.")
            return
        self._create_scheduling_check(
            job["head_sha"],
            "action_required",
            f"{self._preemption_summary(job)} "
            "Requeue the resource job manually; this is not a test failure.",
        )
        self.summarize(
            "Action required: scheduling retry budget exhausted. "
            f"{self._preemption_summary(job)} Requeue the resource job manually."
        )

    def _resolve_action_required(self, job: JsonObject, head_repository: str) -> None:
        if not self._is_current_attempt(job):
            return
        if not self._has_open_pr(job["head_sha"], head_repository):
            return
        existing = self._latest_scheduling_check(job["head_sha"])
        if existing is None or existing["conclusion"] != "action_required":
            return
        self.client.patch(
            f"/check-runs/{existing['id']}",
            {
                "status": "completed",
                "conclusion": "success",
                "output": {
                    "title": "AgentSight resource scheduling",
                    "summary": "The AgentSight resource test passed after scheduling retries.",
                },
            },
        )
        self.summarize("AgentSight resource scheduling recovered: the test passed.")

    def _has_open_pr(self, head_sha: str, head_repository: str) -> bool:
        pull_requests = self.client.get(
            f"/commits/{head_sha}/pulls", repository=head_repository
        )
        return any(
            pr["state"] == "open" and pr["head"]["sha"] == head_sha
            for pr in pull_requests
        )


def main() -> int:
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    repository = os.environ.get("GITHUB_REPOSITORY")
    run_id = os.environ.get("WORKFLOW_RUN_ID")
    if not token or not repository or not run_id:
        raise SystemExit(
            "GITHUB_TOKEN, GITHUB_REPOSITORY and WORKFLOW_RUN_ID are required"
        )
    client = GitHubClient(token, repository)
    return ResourceRetry(client, run_id).run()


if __name__ == "__main__":
    raise SystemExit(main())
