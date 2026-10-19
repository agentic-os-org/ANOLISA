"""Decision and loading contract of the secret gateway addon, without mitmproxy."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "v2" / "fixtures" / "gateway"
sys.path.insert(0, str(ROOT / "v2" / "gateway" / "addon"))

import asc_gateway_policy as policy  # noqa: E402

DECISIONS = json.loads((FIXTURES / "decisions.json").read_text())
PLAN = json.loads((FIXTURES / DECISIONS["plan"]).read_text())
VALUES = DECISIONS["values"]


def plan_for(agent: str) -> dict[str, object]:
    plan = copy.deepcopy(PLAN)
    plan["subject"]["name"] = agent
    return plan


def runtime_files(
    secrets: object | None = None,
    plans: dict[str, object] | None = None,
    generation: int = 1,
) -> dict[str, bytes]:
    files = {
        "secrets-1.json": json.dumps(
            DECISIONS["secrets"] if secrets is None else secrets
        ).encode()
    }
    listeners = []
    for listener in DECISIONS["listeners"]:
        name = f"plan-{listener['agent']}.json"
        plan = (plans or {}).get(listener["agent"], plan_for(listener["agent"]))
        files[name] = json.dumps(plan).encode()
        listeners.append({**listener, "plan": name})
    manifest = {
        "version": 1,
        "generation": generation,
        "secrets": "secrets-1.json",
        "listeners": listeners,
    }
    files["active.json"] = json.dumps(manifest).encode()
    return files


def load(files: dict[str, bytes]) -> policy.Snapshot:
    def read(name: str, limit: int) -> bytes:
        data = files[name]
        if len(data) > limit:
            raise policy.LoadError("too large")
        return data

    return policy.build_snapshot(files["active.json"], read)


def facts(document: dict[str, object]) -> policy.RequestFacts:
    return policy.RequestFacts(
        port=document["port"],
        tls=document["tls"],
        sni=document["sni"],
        authority=document["authority"],
        target_authority=document["targetAuthority"],
        method=document["method"],
        path=document["path"],
        headers=tuple((name, value) for name, value in document["headers"]),
        body=document["body"].encode(),
    )


class DecisionMatrixTest(unittest.TestCase):
    def test_fixture_cases(self) -> None:
        snapshot = load(runtime_files())
        for case in DECISIONS["cases"]:
            with self.subTest(case["name"]):
                decision = policy.decide_request(snapshot, facts(case["facts"]))
                expect = case["expect"]
                self.assertEqual(decision.action, expect["action"])
                self.assertEqual(decision.code, expect["code"])
                if expect["action"] == policy.ACTION_INJECT:
                    self.assertEqual(decision.rule, expect["rule"])
                    self.assertEqual(decision.authorization, expect["authorization"])
                    self.assertEqual(
                        decision.response_rewrite, expect["responseRewrite"]
                    )
                    self.assertEqual(
                        decision.replacements, ((VALUES["real"], VALUES["fake"]),)
                    )
                else:
                    self.assertIsNone(decision.authorization)

    def test_no_snapshot_rejects_everything(self) -> None:
        for case in DECISIONS["cases"]:
            decision = policy.decide_request(None, facts(case["facts"]))
            self.assertEqual(
                (decision.action, decision.code), ("reject", "gateway_not_ready")
            )

    def test_request_default_block(self) -> None:
        plan = plan_for("dev-agent")
        plan["requestDefault"] = "block"
        snapshot = load(runtime_files(plans={"dev-agent": plan}))
        case = next(
            c for c in DECISIONS["cases"] if c["expect"]["code"] == "no_credential"
        )
        self.assertEqual(
            policy.decide_request(snapshot, facts(case["facts"])).code,
            "request_blocked",
        )

    def test_values_never_appear_in_reprs(self) -> None:
        snapshot = load(runtime_files())
        decision = policy.decide_request(
            snapshot, facts(DECISIONS["cases"][0]["facts"])
        )
        text = repr(snapshot) + repr(decision) + repr(list(snapshot.live.values()))
        for value in VALUES.values():
            self.assertNotIn(value, text)


class LoadTest(unittest.TestCase):
    def assert_load_error(self, files: dict[str, bytes]) -> str:
        with self.assertRaises(policy.LoadError) as raised:
            load(files)
        message = str(raised.exception)
        for value in VALUES.values():
            self.assertNotIn(value, message)
        return message

    def test_generation_mismatch(self) -> None:
        self.assert_load_error(runtime_files(generation=2))

    def test_plan_subject_must_match_listener(self) -> None:
        self.assert_load_error(
            runtime_files(plans={"dev-agent": plan_for("other-agent")})
        )

    def test_plan_credential_needs_a_fake_for_its_agent(self) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"] = [
            e for e in secrets["entries"] if e["agent"] != "other-agent"
        ]
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_unknown_fields_are_rejected(self) -> None:
        plan = plan_for("dev-agent")
        plan["rules"][0]["match"]["query"] = "x"
        self.assert_load_error(runtime_files(plans={"dev-agent": plan}))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][0]["extra"] = 1
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_invalid_rule_values_are_rejected(self) -> None:
        edits = [
            ("host", "*.github.com"),
            ("path", "/a/../b"),
            ("path", "/a%2fb"),
            ("scheme", "http"),
        ]
        for key, value in edits:
            with self.subTest(key=key, value=value):
                plan = plan_for("dev-agent")
                plan["rules"][0]["match"][key] = value
                self.assert_load_error(runtime_files(plans={"dev-agent": plan}))
        plan = plan_for("dev-agent")
        plan["rules"][0]["match"]["method"]["in"] = ["POST", "GET"]
        self.assert_load_error(runtime_files(plans={"dev-agent": plan}))
        plan = plan_for("dev-agent")
        plan["credentialRequestDefault"] = "allow"
        self.assert_load_error(runtime_files(plans={"dev-agent": plan}))

    def test_because_control_characters_are_rejected(self) -> None:
        plan = plan_for("dev-agent")
        plan["rules"][0]["because"] = "line\nbreak"
        self.assert_load_error(runtime_files(plans={"dev-agent": plan}))

    def test_duplicate_json_keys_are_rejected(self) -> None:
        files = runtime_files()
        text = files["plan-dev-agent.json"].decode()
        files["plan-dev-agent.json"] = text.replace(
            '"requestDefault": "allow"',
            '"requestDefault": "block", "requestDefault": "allow"',
        ).encode()
        self.assertIn(b'"requestDefault": "block"', files["plan-dev-agent.json"])
        self.assert_load_error(files)

    def test_duplicate_and_overlapping_values_are_rejected(self) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["retired"][0]["fake"] = VALUES["fake"]
        self.assert_load_error(runtime_files(secrets=secrets))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][1]["real"] = VALUES["fake"]
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_file_names_must_be_plain(self) -> None:
        files = runtime_files()
        manifest = json.loads(files["active.json"])
        manifest["secrets"] = "../secrets-1.json"
        files["active.json"] = json.dumps(manifest).encode()
        self.assert_load_error(files)

    def test_duplicate_ports_are_rejected(self) -> None:
        files = runtime_files()
        manifest = json.loads(files["active.json"])
        manifest["listeners"][1]["port"] = manifest["listeners"][0]["port"]
        files["active.json"] = json.dumps(manifest).encode()
        self.assert_load_error(files)


class PathPatternTest(unittest.TestCase):
    def test_globs(self) -> None:
        pattern = policy.compile_path_pattern("/**/info/refs")
        self.assertTrue(pattern.match("/o/r.git/info/refs"))
        self.assertTrue(pattern.match("/g/sub/r.git/info/refs"))
        self.assertFalse(pattern.match("/info/refs"))
        self.assertFalse(pattern.match("/o/r.git/info/refs/x"))
        single = policy.compile_path_pattern("/repos/*/issues")
        self.assertTrue(single.match("/repos/o/issues"))
        self.assertFalse(single.match("/repos/o/r/issues"))
        self.assertTrue(policy.compile_path_pattern("/**").match("/"))


class RewriteTest(unittest.TestCase):
    def test_rewrite_maps_real_to_fake(self) -> None:
        pairs = ((VALUES["real"], VALUES["fake"]),)
        body = f'{{"token":"{VALUES["real"]}","again":"{VALUES["real"]}"}}'.encode()
        rewritten = policy.rewrite_bytes(body, pairs)
        self.assertNotIn(VALUES["real"].encode(), rewritten)
        self.assertEqual(len(rewritten), len(body))
        self.assertEqual(rewritten.count(VALUES["fake"].encode()), 2)


if __name__ == "__main__":
    unittest.main()
