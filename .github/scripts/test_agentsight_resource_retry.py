"""Behavior and workflow-contract tests for AgentSight resource scheduling."""

from __future__ import annotations

import unittest
from pathlib import Path
from typing import Any

from retry_agentsight_resource import JOB_NAME, ResourceRetry

ROOT = Path(__file__).resolve().parents[2]
HEAD = "a" * 40
RUN_ID = "123"
JOB_ID = 900
PREEMPTION_MESSAGE = (
    "Canceling since a higher priority waiting request for "
    "agentic-os-org/ANOLISA-agentsight-resource-kernel66 exists"
)


class FakeClient:
    repository = "agentic-os-org/ANOLISA"

    def __init__(
        self,
        attempt: int = 1,
        conclusion: str = "cancelled",
        event: str = "pull_request",
        annotation: str | None = PREEMPTION_MESSAGE,
        latest_id: int = JOB_ID,
        pr_state: str = "open",
        post_error: bool = False,
        steps: list[dict[str, Any]] | None = None,
        steps_null: bool = False,
        head_repository: str = "agentic-os-org/ANOLISA",
        scheduling_checks: list[dict[str, Any]] | None = None,
        base_pulls: list[dict[str, Any]] | None = None,
        fork_pulls: list[dict[str, Any]] | None = None,
    ) -> None:
        self.responses: dict[str, Any] = {
            f"/actions/runs/{RUN_ID}": {
                "event": event,
                "run_attempt": attempt,
                "head_repository": {"full_name": head_repository},
            },
            f"/actions/runs/{RUN_ID}/attempts/{attempt}/jobs?per_page=100": {
                "jobs": [
                    {
                        "id": JOB_ID,
                        "name": JOB_NAME,
                        "conclusion": conclusion,
                        "run_attempt": attempt,
                        "head_sha": HEAD,
                        "steps": None if steps_null else (steps or []),
                    }
                ]
            },
            f"/check-runs/{JOB_ID}/annotations": [{"message": annotation}],
            f"/commits/{HEAD}/check-runs?per_page=100": {
                "check_runs": [
                    {"id": latest_id, "name": JOB_NAME, "conclusion": conclusion},
                    *(scheduling_checks or []),
                ]
            },
            f"/commits/{HEAD}/pulls": [{"state": pr_state, "head": {"sha": HEAD}}],
        }
        self.pulls_by_repository = {
            "agentic-os-org/ANOLISA": (
                base_pulls
                if base_pulls is not None
                else [{"state": pr_state, "head": {"sha": HEAD}}]
            ),
            "Daydreamer-Li/anolisa": (
                fork_pulls
                if fork_pulls is not None
                else [{"state": pr_state, "head": {"sha": HEAD}}]
            ),
        }
        self.post_error = post_error
        self.posts: list[tuple[str, dict[str, Any] | None]] = []
        self.patches: list[tuple[str, dict[str, Any]]] = []
        self.last_repository = self.repository

    def get(self, path: str, repository: str | None = None) -> Any:
        self.last_repository = repository or self.repository
        if path == f"/commits/{HEAD}/pulls":
            return self.pulls_by_repository[self.last_repository]
        return self.responses[path]

    def post(self, path: str, payload: dict[str, Any] | None = None) -> int:
        if self.post_error:
            raise RuntimeError("injected rerun failure")
        self.posts.append((path, payload))
        return 201

    def patch(self, path: str, payload: dict[str, Any]) -> int:
        self.patches.append((path, payload))
        return 200


class ResourceRetryTests(unittest.TestCase):
    def run_retry(self, client: FakeClient) -> tuple[list[float], list[str]]:
        sleeps: list[float] = []
        messages: list[str] = []
        result = ResourceRetry(client, RUN_ID, sleeps.append, messages.append).run()
        self.assertEqual(result, 0)
        return sleeps, messages

    def test_first_preemption_retries_after_one_minute(self) -> None:
        client = FakeClient()
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [60])
        self.assertEqual(client.posts, [(f"/actions/jobs/{JOB_ID}/rerun", None)])
        self.assertTrue(any("Retry 1/3 scheduled" in message for message in messages))
        self.assertTrue(
            any("before any test steps ran" in message for message in messages)
        )

    def test_backoff_escalates_across_attempts(self) -> None:
        for attempt, expected in ((2, 300), (3, 900)):
            with self.subTest(attempt=attempt):
                client = FakeClient(attempt=attempt)
                sleeps, messages = self.run_retry(client)
                self.assertEqual(sleeps, [expected])
                self.assertTrue(
                    any(
                        f"Retry {attempt}/3 requeued" in message for message in messages
                    )
                )

    def test_retry_budget_exhaustion_is_action_required_not_test_failure(self) -> None:
        client = FakeClient(attempt=4)
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts[0][0], "/check-runs")
        self.assertEqual(client.posts[0][1]["conclusion"], "action_required")
        self.assertEqual(client.posts[0][1]["head_sha"], HEAD)
        self.assertEqual(client.posts[0][1]["name"], "AgentSight resource scheduling")
        self.assertTrue(any("Action required" in message for message in messages))
        self.assertTrue(
            any("before any test steps ran" in message for message in messages)
        )

    def test_running_exhaustion_reports_no_verdict_not_no_steps(self) -> None:
        client = FakeClient(
            attempt=4,
            steps=[{"name": "Measure resource usage", "conclusion": "cancelled"}],
        )
        self.run_retry(client)
        summary = client.posts[0][1]["output"]["summary"]
        self.assertIn("before producing a test verdict", summary)
        self.assertNotIn("before any test steps ran", summary)

    def test_real_failure_is_not_retried(self) -> None:
        client = FakeClient(conclusion="failure")
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("was not cancelled by scheduling" in message for message in messages)
        )

    def test_other_cancellation_is_not_retried(self) -> None:
        client = FakeClient(annotation="The operation was canceled by a user")
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_completed_steps_are_not_retried_or_relabelled(self) -> None:
        client = FakeClient(
            steps=[{"name": "Measure resource usage", "conclusion": "failure"}]
        )
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("test or validation step failed" in message for message in messages)
        )

    def test_running_preemption_is_retried_without_false_no_steps_claim(self) -> None:
        client = FakeClient(
            steps=[
                {"name": "Set up job", "conclusion": "success"},
                {"name": "Measure resource usage", "conclusion": "cancelled"},
            ]
        )
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [60])
        self.assertEqual(client.posts, [(f"/actions/jobs/{JOB_ID}/rerun", None)])
        self.assertTrue(
            any("before producing a test verdict" in message for message in messages)
        )
        self.assertFalse(any("No test steps" in message for message in messages))

    def test_cleanup_failure_from_cancelled_measurement_is_retryable(self) -> None:
        client = FakeClient(
            steps=[
                {"name": "Measure resource usage", "conclusion": "cancelled"},
                {
                    "name": "Enforce resource limits and publish summary",
                    "conclusion": "failure",
                },
            ]
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [60])

    def test_pre_measurement_preemption_cleanup_failure_is_retryable(self) -> None:
        client = FakeClient(
            steps=[
                {"name": "Install resource test dependencies", "conclusion": "success"},
                {"name": "Measure resource usage", "conclusion": "skipped"},
                {
                    "name": "Enforce resource limits and publish summary",
                    "conclusion": "failure",
                },
            ]
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [60])

    def test_enforce_timeout_with_cancelled_measurement_is_not_retried(self) -> None:
        client = FakeClient(
            steps=[
                {"name": "Measure resource usage", "conclusion": "cancelled"},
                {
                    "name": "Enforce resource limits and publish summary",
                    "conclusion": "timed_out",
                },
            ]
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_failed_verdict_step_is_not_retried(self) -> None:
        client = FakeClient(
            steps=[
                {
                    "name": "Test resource report and limit checks",
                    "conclusion": "failure",
                },
                {"name": "Measure resource usage", "conclusion": "cancelled"},
            ]
        )
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("test or validation step failed" in message for message in messages)
        )

    def test_enforce_failure_with_successful_measurement_is_not_retried(self) -> None:
        client = FakeClient(
            steps=[
                {"name": "Measure resource usage", "conclusion": "success"},
                {
                    "name": "Enforce resource limits and publish summary",
                    "conclusion": "failure",
                },
            ]
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_missing_execution_state_is_not_retried(self) -> None:
        client = FakeClient()
        jobs_path = f"/actions/runs/{RUN_ID}/attempts/1/jobs?per_page=100"
        del client.responses[jobs_path]["jobs"][0]["steps"]
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("execution state is unavailable" in message for message in messages)
        )

    def test_null_annotation_message_is_skipped_safely(self) -> None:
        client = FakeClient(annotation=None)
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_preemption_message_must_match_the_resource_group_exactly(self) -> None:
        client = FakeClient(
            annotation="higher priority waiting request for another job exists"
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_closed_pr_is_not_retried(self) -> None:
        client = FakeClient(pr_state="closed")
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_newer_attempt_prevents_duplicate_rerun(self) -> None:
        client = FakeClient(latest_id=JOB_ID + 1)
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("newer resource attempt" in message for message in messages)
        )

    def test_exhaustion_does_not_update_a_superseded_attempt(self) -> None:
        client = FakeClient(attempt=4, latest_id=JOB_ID + 1)
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])

    def test_lease_is_checked_again_after_backoff(self) -> None:
        client = FakeClient()
        checks_path = f"/commits/{HEAD}/check-runs?per_page=100"

        def replace_attempt(_: float) -> None:
            client.responses[checks_path]["check_runs"][0]["id"] = JOB_ID + 1

        messages: list[str] = []
        result = ResourceRetry(client, RUN_ID, replace_attempt, messages.append).run()
        self.assertEqual(result, 0)
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("newer resource attempt" in message for message in messages)
        )

    def test_fork_pr_uses_head_repository_for_pr_lookup(self) -> None:
        client = FakeClient(
            head_repository="Daydreamer-Li/anolisa",
            base_pulls=[],
            fork_pulls=[{"state": "open", "head": {"sha": HEAD}}],
        )
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [60])
        self.assertEqual(client.last_repository, "Daydreamer-Li/anolisa")
        self.assertEqual(client.posts, [(f"/actions/jobs/{JOB_ID}/rerun", None)])

    def test_exhaustion_check_is_idempotent(self) -> None:
        client = FakeClient(
            attempt=4,
            scheduling_checks=[
                {
                    "id": JOB_ID + 1,
                    "name": "AgentSight resource scheduling",
                    "conclusion": "action_required",
                }
            ],
        )
        self.run_retry(client)
        self.assertEqual(client.posts, [])

    def test_success_resolves_existing_action_required_check(self) -> None:
        client = FakeClient(
            conclusion="success",
            scheduling_checks=[
                {
                    "id": JOB_ID + 1,
                    "name": "AgentSight resource scheduling",
                    "conclusion": "action_required",
                }
            ],
        )
        self.run_retry(client)
        self.assertEqual(client.posts, [])
        self.assertEqual(client.patches[0][0], f"/check-runs/{JOB_ID + 1}")
        self.assertEqual(client.patches[0][1]["conclusion"], "success")

    def test_success_without_action_required_creates_no_check(self) -> None:
        client = FakeClient(conclusion="success")
        self.run_retry(client)
        self.assertEqual(client.posts, [])

    def test_null_steps_is_not_retried(self) -> None:
        client = FakeClient(steps_null=True)
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any("execution state is unavailable" in message for message in messages)
        )

    def test_null_head_repository_falls_back_to_base(self) -> None:
        client = FakeClient()
        client.responses[f"/actions/runs/{RUN_ID}"]["head_repository"] = None
        sleeps, _ = self.run_retry(client)
        self.assertEqual(sleeps, [60])
        self.assertEqual(client.last_repository, "agentic-os-org/ANOLISA")

    def test_missing_current_check_is_skipped_without_workflow_failure(self) -> None:
        client = FakeClient()
        client.responses[f"/commits/{HEAD}/check-runs?per_page=100"]["check_runs"] = []
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(
            any(
                "current resource check cannot be found" in message
                for message in messages
            )
        )

    def test_push_run_is_not_retried(self) -> None:
        client = FakeClient(event="push")
        sleeps, messages = self.run_retry(client)
        self.assertEqual(sleeps, [])
        self.assertEqual(client.posts, [])
        self.assertTrue(any("not a pull_request" in message for message in messages))

    def test_rerun_api_failure_is_visible(self) -> None:
        client = FakeClient(post_error=True)
        with self.assertRaisesRegex(RuntimeError, "injected rerun failure"):
            ResourceRetry(client, RUN_ID, lambda _: None, lambda _: None).run()

    def test_invalid_run_id_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "invalid workflow run id"):
            ResourceRetry(FakeClient(), "../../etc/passwd")

    def test_workflow_permission_and_checkout_contract(self) -> None:
        workflow = (
            ROOT / ".github/workflows/agentsight-resource-retry.yml"
        ).read_text()
        self.assertIn('workflows: ["CI / Components"]', workflow)
        self.assertIn("types: [completed]", workflow)
        self.assertIn("actions: write", workflow)
        self.assertIn("checks: write", workflow)
        self.assertNotIn("checks: read", workflow)
        self.assertEqual(workflow.count("checks:"), 1)
        self.assertIn("contents: read", workflow)
        self.assertIn("pull-requests: read", workflow)
        self.assertNotIn("pull-requests: write", workflow)
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("github.event.workflow_run.head_branch", workflow)
        self.assertNotIn("ref: ${{ github.event.workflow_run", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("${{ github.repository }}-agentsight-resource-retry", workflow)
        group = next(
            line for line in workflow.splitlines() if line.startswith("  group:")
        )
        self.assertNotIn("github.event.workflow_run.id", group)

    def test_governance_runs_the_scheduler_contract_suite(self) -> None:
        workflow = (ROOT / ".github/workflows/repository-metadata.yml").read_text()
        self.assertIn("test_agentsight_resource_retry.py", workflow)


if __name__ == "__main__":
    unittest.main()
