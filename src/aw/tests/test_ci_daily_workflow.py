"""Exercise the parallel daily workflow's dependencies and result wiring."""

import re

from test_ci_checks import AW, GateFixture, gate


class DailyWorkflowTests(GateFixture):
    def setUp(self) -> None:
        super().setUp()
        workflow = (AW.parents[1] / ".github/workflows/aw-ci.yml").read_text()
        self.triggers, jobs = workflow.split("\njobs:\n", 1)
        self.jobs = dict(re.findall(
            r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)", jobs, re.MULTILINE | re.DOTALL,
        ))
        self.sha = "a" * 40

    def result_environment(
        self, states: dict[str, str], selected: str = "true", tokenless: str = "true",
    ) -> dict[str, str]:
        needs = {name: {"result": result, "outputs": {}} for name, result in states.items()}
        needs["scope"]["outputs"] = {
            "selected": selected, "tokenless": tokenless, "candidate_sha": self.sha,
        }
        for job in ("checks", "tokenless"):
            # Outputs can exist even when a later step or artifact upload fails.
            outputs = re.search(r"^    outputs:\n(.*?)(?=^    \w)", self.jobs[job],
                                re.MULTILINE | re.DOTALL).group(1)
            for name, step, field in re.findall(
                    r"^      (\w+): \$\{\{ steps\.(\w+)\.outputs\.(\w+) \}\}$",
                    outputs, re.MULTILINE):
                needs[job]["outputs"][name] = (
                    self.sha if step == job and field == "tested_sha" else ""
                )
        env = {"GITHUB_SHA": self.sha}
        for name, job, path in re.findall(
                r"^          (\w+): \$\{\{ needs\.(\w+)\.([\w.]+) \}\}$",
                self.jobs["required"], re.MULTILINE):
            value = needs[job]
            for part in path.split("."):
                value = value.get(part, "") if isinstance(value, dict) else ""
            env[name] = value
        return env

    def test_workers_are_independent_and_required_always_joins_both(self) -> None:
        for job, output in (("checks", "selected"), ("tokenless", "tokenless")):
            block = self.jobs[job]
            self.assertRegex(block, r"(?m)^    needs: scope$")
            self.assertRegex(block, rf"(?m)^    if: needs.scope.outputs.{output} == 'true'$")
            self.assertIn("ref: ${{ github.sha }}", block)
            self.assertNotIn("continue-on-error:", block)
        required = self.jobs["required"]
        self.assertIn("    name: AW / required\n", required)
        self.assertIn("    needs: [scope, checks, tokenless]\n", required)
        self.assertIn("    if: always()\n", required)
        self.assertIn("ref: ${{ github.sha }}", required)
        self.assertIn("run: python3 src/aw/scripts/check.py required", required)
        self.assertNotIn("continue-on-error:", required)
        # Only scope compares base and head. Workers test the selected checkout.
        self.assertIn("fetch-depth: 0", self.jobs["scope"])
        self.assertNotIn("fetch-depth: 0", self.jobs["checks"])

    def test_manual_acceptance_does_not_cancel_branch_or_pr_checks(self) -> None:
        concurrency = self.triggers.split("\nconcurrency:\n", 1)[1].split("\nenv:\n", 1)[0]
        self.assertIn("${{ github.workflow }}-${{ github.event_name }}", concurrency)
        self.assertIn("github.event_name == 'workflow_dispatch' && github.run_id || "
                      "github.event.pull_request.number || github.ref", concurrency)
        self.assertIn("cancel-in-progress: true", concurrency)

    def test_contract_build_and_cache_share_a_single_target(self) -> None:
        checks = self.jobs["checks"]
        self.assertIn(
            "          workspaces: |\n"
            "            src/aw -> target/cargo-contracts\n"
            "            providers/agent-sec-core\n"
            "            providers/tokenless\n",
            checks,
        )
        self.assertIn("CARGO_TARGET_DIR: ${{ github.workspace }}/src/aw/target/cargo-contracts", checks)

    def test_only_cold_pr_and_queue_builds_override_the_tokenless_link_profile(self) -> None:
        tokenless = self.jobs["tokenless"]
        restore, daily = tokenless.split("      - name: Reduce Tokenless link time", 1)
        daily, validate = daily.split("      - name: Validate real Tokenless", 1)
        self.assertIn("id: tokenless-cache", restore)
        self.assertIn("save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
                      restore)
        self.assertIn("if: (github.event_name == 'pull_request' || github.event_name == 'merge_group') "
                      "&& steps.tokenless-cache.outputs.cache-hit != 'true'", daily)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=false", daily)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16", daily)
        self.assertIn("src/aw/scripts/check_tokenless.py", validate)
        self.assertNotIn("CARGO_PROFILE_RELEASE_", self.triggers + restore + validate)

    def test_contract_checks_use_a_compatible_trusted_system_python(self) -> None:
        checks = self.jobs["checks"]
        self.assertIn("    runs-on: ubuntu-24.04\n", checks)
        self.assertIn('ln -s /usr/bin/python3 "$aw_native_tools_dir/python3"', checks)
        self.assertIn("assert sys.version_info >= (3, 11)", checks)
        self.assertIn('"${pythonLocation}/bin/python3" src/aw/scripts/check.py', checks)

    def test_required_accepts_only_selected_success_and_explicit_noop(self) -> None:
        for selected, tokenless in (("true", "true"), ("true", "false"), ("false", "true"), ("false", "false")):
            states = {"scope": "success", "checks": "success" if selected == "true" else "skipped",
                      "tokenless": "success" if tokenless == "true" else "skipped"}
            with self.subTest(selected=selected, tokenless=tokenless):
                gate.required(self.result_environment(states, selected, tokenless))

    def test_required_rejects_failed_cancelled_or_missing_jobs_with_published_outputs(self) -> None:
        success = {"scope": "success", "checks": "success", "tokenless": "success"}
        for job in success:
            for result in ("failure", "cancelled", "skipped", ""):
                with self.subTest(job=job, result=result), self.assertRaises(ValueError):
                    gate.required(self.result_environment({**success, job: result}))

    def test_required_rejects_missing_or_stale_candidate_and_worker_outputs(self) -> None:
        env = self.result_environment({"scope": "success", "checks": "success", "tokenless": "success"})
        for field in ("CANDIDATE_SHA", "TESTED_SHA", "TOKENLESS_TESTED_SHA"):
            for value in ("", "b" * 40):
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    gate.required({**env, field: value})

