#!/usr/bin/env python3
"""Regression tests for repository governance metadata validation."""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any, Callable

SOURCE_ROOT = Path(__file__).resolve().parents[2]


class RepositoryMetadataValidationTest(unittest.TestCase):
    """Run the validator against isolated metadata mutations."""

    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name) / "repository"
        shutil.copytree(SOURCE_ROOT / ".github", self.root / ".github")
        self.validator = self.root / ".github/scripts/validate-repository-metadata.py"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def run_validator(self) -> subprocess.CompletedProcess[str]:
        """Execute the copied validator and capture its diagnostics."""
        return subprocess.run(
            ["python3", str(self.validator)],
            cwd=self.root,
            check=False,
            capture_output=True,
            text=True,
        )

    def mutate_components(self, mutation: Callable[[dict[str, Any]], None]) -> None:
        """Apply one test mutation to the copied component metadata."""
        path = self.root / ".github/components.json"
        document = json.loads(path.read_text(encoding="utf-8"))
        mutation(document)
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")

    def component(self, component_id: str) -> dict[str, Any]:
        path = self.root / ".github/components.json"
        document = json.loads(path.read_text(encoding="utf-8"))
        return next(c for c in document["components"] if c["id"] == component_id)

    def update_component(self, component_id: str, **changes: Any) -> None:
        self.mutate_components(
            lambda document: next(
                c for c in document["components"] if c["id"] == component_id
            ).update(changes)
        )

    def use_layout(self, migrated: bool) -> None:
        """Relocate metadata, ownership and CI routes only in the fixture."""
        replacements: dict[str, str] = {}
        paths = {
            "anolisa": "distribution/anolisa/" if migrated else "src/anolisa/",
            "cosh": "deprecated/copilot-shell/" if migrated else "src/copilot-shell/",
            "benchmark": "benchmark/" if migrated else "src/benchmark/",
        }

        def relocate(document: dict[str, Any]) -> None:
            if not any(c["id"] == "benchmark" for c in document["components"]):
                # Benchmark is currently unregistered and has no release or CI job.
                document["components"].append({
                    "id": "benchmark",
                    "display_name": "Benchmark",
                    "aliases": [],
                    "issue_option": None,
                    "path_prefixes": ["src/benchmark/"],
                    "label": "component:benchmark",
                    "commit_scope": None,
                    "commit_scope_aliases": [],
                    "release_tag_prefix": None,
                    "ci_key": None,
                    "issue_triagers": [],
                    "status": "internal",
                })
            for component in document["components"]:
                if component["id"] in paths:
                    old_path, = component["path_prefixes"]
                    new_path = paths[component["id"]]
                    replacements[old_path] = new_path
                    component["path_prefixes"] = [new_path]
                if component["id"] == "cosh":
                    component["status"] = "deprecated" if migrated else "active"

        self.mutate_components(relocate)
        for relative_path in ("CODEOWNERS", "workflows/ci.yaml"):
            path = self.root / ".github" / relative_path
            text = path.read_text(encoding="utf-8")
            marker = "/" if relative_path == "CODEOWNERS" else "^"
            for old_path, new_path in replacements.items():
                text = text.replace(f"{marker}{old_path}", f"{marker}{new_path}")
            if relative_path == "CODEOWNERS" and "component:benchmark" not in text:
                text += f"/{paths['benchmark']} @benchmark-maintainer # auto-label: component:benchmark\n"
            path.write_text(text, encoding="utf-8")

    def assert_validation_error(self, expected: str) -> str:
        """Require validation failure containing the expected diagnostic."""
        result = self.run_validator()
        output = result.stdout + result.stderr
        self.assertNotEqual(result.returncode, 0, output)
        self.assertIn(expected, output)
        return output

    def test_committed_metadata_is_valid(self) -> None:
        result = self.run_validator()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_legacy_layout_is_valid(self) -> None:
        self.use_layout(migrated=False)
        result = self.run_validator()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_migrated_layout_preserves_component_identities(self) -> None:
        originals = [self.component(component_id) for component_id in ("anolisa", "cosh")]
        release_path = self.root / ".github/workflows/release.yaml"
        release_text = release_path.read_text(encoding="utf-8")
        self.use_layout(migrated=True)
        for original in originals:
            migrated = self.component(original["id"])
            for field, value in original.items():
                if field not in {"path_prefixes", "status"}:
                    self.assertEqual(migrated[field], value, field)
        self.assertEqual(self.component("anolisa")["path_prefixes"], ["distribution/anolisa/"])
        self.assertEqual(self.component("cosh")["path_prefixes"], ["deprecated/copilot-shell/"])
        self.assertEqual(self.component("benchmark")["path_prefixes"], ["benchmark/"])
        self.assertEqual(self.component("cosh")["status"], "deprecated")
        self.assertEqual(release_path.read_text(encoding="utf-8"), release_text)
        result = self.run_validator()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_invalid_component_paths_are_rejected(self) -> None:
        invalid_paths = [
            "", "/", "anolisa/", "vendor/anolisa/", "other/benchmark/",
            "benchmark", "/benchmark/", "benchmark//", "benchmark/../",
            "benchmark/anolisa/", "./benchmark/", "../benchmark/",
        ]
        for root in ("src", "distribution", "deprecated"):
            invalid_paths.extend([
                f"/{root}/anolisa/", f"../{root}/anolisa/", f"./{root}/anolisa/",
                f"{root}/", f"{root}//", f"{root}/anolisa", f"{root}//anolisa/",
                f"{root}/anolisa//", f"{root}/./", f"{root}/../",
                f"{root}/../anolisa/", f"{root}/anolisa/../", f"{root}/anolisa/./",
                f"{root}/anolisa/nested/", f"{root}/anolisa\\nested/",
                f"{root}/anolisa/*/", f"{root}/anolisa/\n",
            ])
        for prefix in invalid_paths:
            with self.subTest(prefix=prefix):
                self.update_component("anolisa", path_prefixes=[prefix])
                self.assert_validation_error(f"anolisa: invalid path prefix {prefix!r}")

    def test_active_and_deprecated_components_require_public_metadata(self) -> None:
        original = self.component("cosh")
        for status in ("active", "deprecated"):
            for field, article in (
                ("issue_option", "an"),
                ("commit_scope", "a"),
                ("release_tag_prefix", "a"),
            ):
                with self.subTest(status=status, field=field):
                    self.update_component("cosh", **{**original, "status": status, field: None})
                    self.assert_validation_error(
                        f"cosh: {status} components need {article} {field}"
                    )

    def test_deprecated_component_requires_maintainer_scope(self) -> None:
        self.update_component("cosh", status="deprecated")
        path = self.root / ".github/maintainers.json"
        document = json.loads(path.read_text(encoding="utf-8"))
        document["scopes"] = [
            scope for scope in document["scopes"] if scope["label"] != "component:cosh"
        ]
        path.write_text(json.dumps(document), encoding="utf-8")
        self.assert_validation_error("maintainers.json: missing scope for component:cosh")

    def test_deprecated_component_requires_effective_triagers(self) -> None:
        self.update_component("cosh", status="deprecated", issue_triagers=[])
        result = self.run_validator()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.mutate_components(lambda document: document["defaults"].update(issue_triagers=[]))
        self.assert_validation_error("cosh: public components need an effective issue triager")

    def test_deprecated_component_requires_codeowners(self) -> None:
        self.update_component("cosh", status="deprecated")
        prefix = self.component("cosh")["path_prefixes"][0]
        path = self.root / ".github/CODEOWNERS"
        lines = path.read_text(encoding="utf-8").splitlines()
        path.write_text(
            "\n".join(line for line in lines if not line.startswith(f"/{prefix}")) + "\n",
            encoding="utf-8",
        )
        self.assert_validation_error(f"CODEOWNERS: /{prefix} does not map to component:cosh")

    def test_deprecated_component_keeps_original_release_tag(self) -> None:
        self.update_component("cosh", status="deprecated", release_tag_prefix="copilot-shell/v")
        output = self.assert_validation_error(
            "release.yaml: missing trigger prefix for component release prefix 'copilot-shell/v'"
        )
        self.assertIn("release.yaml: unknown trigger prefix 'cosh/v'", output)

    def test_deprecated_component_requires_all_release_contracts(self) -> None:
        self.update_component("cosh", status="deprecated")
        path = self.root / ".github/workflows/release.yaml"
        lines = path.read_text(encoding="utf-8").splitlines()
        for contract, marker in (
            ("trigger prefix", "'cosh/v*'"),
            ("parsed prefix", 'COMPONENT="copilot-shell"'),
            ("published prefix", 'PREFIX="cosh/v"'),
        ):
            with self.subTest(contract=contract):
                path.write_text(
                    "\n".join(line for line in lines if marker not in line) + "\n",
                    encoding="utf-8",
                )
                self.assert_validation_error(
                    f"release.yaml: missing {contract} for component release prefix 'cosh/v'"
                )

    def test_deprecated_component_requires_all_ci_contracts(self) -> None:
        self.update_component("cosh", status="deprecated")
        path = self.root / ".github/workflows/ci.yaml"
        lines = path.read_text(encoding="utf-8").splitlines()
        for contract, marker in (
            ("declared output", "copilot_shell: ${{ steps.changes.outputs.copilot_shell }}"),
            ("emitted output", 'echo "copilot_shell=$COPILOT_SHELL"'),
            ("consumed output", "if: needs.detect-changes.outputs.copilot_shell"),
        ):
            with self.subTest(contract=contract):
                path.write_text(
                    "\n".join(line for line in lines if marker not in line) + "\n",
                    encoding="utf-8",
                )
                self.assert_validation_error(
                    f"ci.yaml: missing {contract} for component CI key 'copilot_shell'"
                )

    def test_deprecated_component_cannot_drop_ci_key(self) -> None:
        self.update_component("cosh", status="deprecated", ci_key=None)
        self.assert_validation_error("ci.yaml: unknown declared output 'copilot_shell'")

    def test_deprecated_component_requires_ci_path_route(self) -> None:
        self.update_component("cosh", status="deprecated")
        prefix = self.component("cosh")["path_prefixes"][0]
        path = self.root / ".github/workflows/ci.yaml"
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                f'"^{prefix}"', '"^deprecated/undeclared/"', 1
            ),
            encoding="utf-8",
        )
        output = self.assert_validation_error(f"ci.yaml: cosh path {prefix!r} has no change route")
        self.assertIn(
            "route pattern '^deprecated/undeclared/' is not an exact declared component path",
            output,
        )

    def test_allowed_roots_do_not_allow_undeclared_downstream_paths(self) -> None:
        codeowners = self.root / ".github/CODEOWNERS"
        ci = self.root / ".github/workflows/ci.yaml"
        original_codeowners = codeowners.read_text(encoding="utf-8")
        original_ci = ci.read_text(encoding="utf-8")
        prefix = self.component("anolisa")["path_prefixes"][0]
        for undeclared in (
            "src/undeclared/", "distribution/undeclared/", "deprecated/undeclared/", "benchmark/"
        ):
            with self.subTest(prefix=undeclared):
                codeowners.write_text(
                    original_codeowners + f"/{undeclared} @owner # auto-label: component:anolisa\n",
                    encoding="utf-8",
                )
                ci.write_text(
                    original_ci.replace(f'"^{prefix}"', f'"^{undeclared}"', 1),
                    encoding="utf-8",
                )
                output = self.assert_validation_error(
                    f"CODEOWNERS: /{undeclared} is not declared by anolisa path prefixes"
                )
                self.assertIn(
                    f"route pattern '^{undeclared}' is not an exact declared component path", output
                )

    def test_removed_component_leaves_no_silent_downstream_references(self) -> None:
        self.mutate_components(
            lambda document: document.__setitem__(
                "components",
                [component for component in document["components"] if component["id"] != "cosh"],
            )
        )
        output = self.assert_validation_error("unknown component scope 'cosh'")
        self.assertIn("unknown component option 'cosh'", output)
        self.assertIn("unknown component annotation component:cosh", output)
        self.assertIn("unknown component scope component:cosh", output)

    def test_question_form_must_contain_every_component_option(self) -> None:
        path = self.root / ".github/ISSUE_TEMPLATE/question.yml"
        path.write_text(
            path.read_text(encoding="utf-8").replace("        - sight\n", "", 1),
            encoding="utf-8",
        )
        self.assert_validation_error("question.yml: missing component option 'sight'")

    def test_issue_form_option_parsing_accepts_different_indentation(self) -> None:
        path = self.root / ".github/ISSUE_TEMPLATE/question.yml"
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                "        - sight\n", "          - sight\n", 1
            ),
            encoding="utf-8",
        )
        result = self.run_validator()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unknown_component_status_is_rejected(self) -> None:
        def change_status(document: dict[str, Any]) -> None:
            document["components"][0]["status"] = "actve"

        self.mutate_components(change_status)
        self.assert_validation_error("anolisa: unknown status 'actve'")

    def test_each_public_path_requires_its_codeowners_mapping(self) -> None:
        path = self.root / ".github/CODEOWNERS"
        lines = [
            line
            for line in path.read_text(encoding="utf-8").splitlines()
            if "/src/blaze/" not in line
        ]
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        self.assert_validation_error(
            "CODEOWNERS: /src/blaze/ does not map to component:blaze"
        )

    def test_undeclared_path_leaves_no_codeowners_mapping(self) -> None:
        path = self.root / ".github/CODEOWNERS"
        path.write_text(
            path.read_text(encoding="utf-8")
            + "/src/legacy-blaze/ @casparant # auto-label: component:blaze\n",
            encoding="utf-8",
        )
        self.assert_validation_error(
            "CODEOWNERS: /src/legacy-blaze/ is not declared by blaze path prefixes"
        )

    def test_complete_codeowners_annotation_is_validated(self) -> None:
        path = self.root / ".github/CODEOWNERS"
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                "# auto-label: component:cosh",
                "# auto-label: component:cosh!",
                1,
            ),
            encoding="utf-8",
        )
        self.assert_validation_error(
            "CODEOWNERS: unknown component annotation component:cosh!"
        )

    def test_issue_options_must_be_unique(self) -> None:
        def duplicate_issue_option(document: dict[str, Any]) -> None:
            document["components"][1]["issue_option"] = document["components"][0][
                "issue_option"
            ]

        self.mutate_components(duplicate_issue_option)
        self.assert_validation_error("duplicate issue option: anolisa")

    def test_component_aliases_must_not_shadow_component_ids(self) -> None:
        def shadow_component_id(document: dict[str, Any]) -> None:
            document["components"][1]["aliases"].append("anolisa")

        self.mutate_components(shadow_component_id)
        self.assert_validation_error("duplicate component identifier: anolisa")

    def test_component_scopes_must_not_use_reserved_scopes(self) -> None:
        def use_reserved_scope(document: dict[str, Any]) -> None:
            document["components"][1]["commit_scope_aliases"].append("docs")

        self.mutate_components(use_reserved_scope)
        self.assert_validation_error(
            "component commit scope conflicts with reserved scope: 'docs'"
        )

    def test_issue_options_must_not_use_fallback_options(self) -> None:
        def use_fallback_option(document: dict[str, Any]) -> None:
            document["components"][1]["issue_option"] = "other"

        self.mutate_components(use_fallback_option)
        for form_name in ("bug_report.yml", "feature_request.yml", "question.yml"):
            path = self.root / ".github/ISSUE_TEMPLATE" / form_name
            path.write_text(
                path.read_text(encoding="utf-8").replace("        - cosh\n", "", 1),
                encoding="utf-8",
            )
        self.assert_validation_error(
            "component issue option conflicts with fallback option: 'other'"
        )

    def test_commit_scopes_and_aliases_must_be_globally_unique(self) -> None:
        def duplicate_commit_scope(document: dict[str, Any]) -> None:
            document["components"][1]["commit_scope_aliases"].append(
                document["components"][0]["commit_scope"]
            )

        self.mutate_components(duplicate_commit_scope)
        self.assert_validation_error("duplicate commit scope: anolisa")

    def test_ci_keys_must_be_unique(self) -> None:
        def duplicate_ci_key(document: dict[str, Any]) -> None:
            document["components"][1]["ci_key"] = document["components"][0]["ci_key"]

        self.mutate_components(duplicate_ci_key)
        self.assert_validation_error("duplicate CI key: anolisa")

    def test_ci_keys_must_match_the_workflow_contract(self) -> None:
        def change_ci_key(document: dict[str, Any]) -> None:
            document["components"][0]["ci_key"] = "not_a_real_ci_output"

        self.mutate_components(change_ci_key)
        self.assert_validation_error(
            "ci.yaml: missing declared output for component CI key 'not_a_real_ci_output'"
        )

    def test_ci_keys_must_route_to_their_components(self) -> None:
        def swap_ci_keys(document: dict[str, Any]) -> None:
            first, second = document["components"][:2]
            first["ci_key"], second["ci_key"] = second["ci_key"], first["ci_key"]

        self.mutate_components(swap_ci_keys)
        self.assert_validation_error(
            "ci.yaml: anolisa CI key 'copilot_shell' routes through ['anolisa']"
        )

    def test_ci_consumers_in_comments_are_ignored(self) -> None:
        path = self.root / ".github/workflows/ci.yaml"
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                "    if: needs.detect-changes.outputs.anolisa == 'true'",
                "    # if: needs.detect-changes.outputs.anolisa == 'true'\n"
                "    if: false",
                1,
            ),
            encoding="utf-8",
        )
        self.assert_validation_error(
            "ci.yaml: missing consumed output for component CI key 'anolisa'"
        )

    def test_ci_path_routes_in_comments_are_ignored(self) -> None:
        path = self.root / ".github/workflows/ci.yaml"
        prefix = self.component("anolisa")["path_prefixes"][0]
        old_route = (
            f'          if echo "$CHANGED" | grep -q "^{prefix}"; then\n'
            "            ANOLISA=true\n"
            "          fi"
        )
        commented_route = (
            f'          # if echo "$CHANGED" | grep -q "^{prefix}"; '
            "then ANOLISA=true"
        )
        path.write_text(
            path.read_text(encoding="utf-8").replace(old_route, commented_route, 1),
            encoding="utf-8",
        )
        self.assert_validation_error(
            f"ci.yaml: anolisa path {prefix!r} has no change route"
        )

    def test_ci_routes_reject_undeclared_path_alternatives(self) -> None:
        path = self.root / ".github/workflows/ci.yaml"
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                'grep -q "^src/blaze/"',
                'grep -qE "^src/(legacy-blaze|blaze)/"',
                1,
            ),
            encoding="utf-8",
        )
        self.assert_validation_error(
            "ci.yaml: route pattern '^src/(legacy-blaze|blaze)/' "
            "is not an exact declared component path"
        )

    def test_release_prefixes_must_match_the_workflow_contract(self) -> None:
        def change_release_prefix(document: dict[str, Any]) -> None:
            document["components"][0]["release_tag_prefix"] = "not-a-real-prefix/v"

        self.mutate_components(change_release_prefix)
        self.assert_validation_error(
            "release.yaml: missing trigger prefix for component release prefix "
            "'not-a-real-prefix/v'"
        )

    def test_release_prefixes_must_route_to_their_components(self) -> None:
        def swap_release_prefixes(document: dict[str, Any]) -> None:
            first, second = document["components"][:2]
            first["release_tag_prefix"], second["release_tag_prefix"] = (
                second["release_tag_prefix"],
                first["release_tag_prefix"],
            )

        self.mutate_components(swap_release_prefixes)
        self.assert_validation_error(
            "release.yaml: anolisa prefix 'cosh/v' parses as 'copilot-shell'"
        )

    def test_public_component_requires_specific_or_default_triagers(self) -> None:
        def remove_fallback(document: dict[str, Any]) -> None:
            document["defaults"]["issue_triagers"] = []

        self.mutate_components(remove_fallback)
        self.assert_validation_error("ktuner: public components need an effective issue triager")


class MigratedRepositoryMetadataValidationTest(RepositoryMetadataValidationTest):
    """Run the same regression suite against the complete three-directory fixture."""

    def setUp(self) -> None:
        super().setUp()
        self.use_layout(migrated=True)


if __name__ == "__main__":
    unittest.main()
