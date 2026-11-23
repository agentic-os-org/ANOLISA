"""Decision and loading contract of the secret gateway addon, without mitmproxy."""

from __future__ import annotations

import base64
import copy
import json
import random
import sys
import tracemalloc
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "v2" / "fixtures" / "gateway"
sys.path.insert(0, str(ROOT / "v2" / "gateway" / "addon"))

import asc_gateway_policy as policy  # noqa: E402

DECISIONS = json.loads((FIXTURES / "decisions.json").read_text())
PLAN = json.loads((FIXTURES / DECISIONS["plan"]).read_text())
VALUES = DECISIONS["values"]
API_FACTS = next(
    case["facts"]
    for case in DECISIONS["cases"]
    if case["name"] == "api bearer is injected"
)


def plan_for(agent: str) -> dict[str, object]:
    plan = copy.deepcopy(PLAN)
    plan["scopeId"] = agent
    if agent == DECISIONS["listeners"][1]["scopeId"]:
        plan["credentials"] = ["pair-other"]
        for rule in plan["rules"]:
            rule["match"]["credentialId"] = "pair-other"
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
        name = f"plan-{listener['scopeId']}.json"
        plan = (plans or {}).get(listener["scopeId"], plan_for(listener["scopeId"]))
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
    )


class DecisionMatrixTest(unittest.TestCase):
    def test_fixture_cases(self) -> None:
        snapshot = load(runtime_files())
        for case in DECISIONS["cases"]:
            with self.subTest(case["name"]):
                selected = snapshot
                if "planRuleIndices" in case:
                    plan = plan_for(PLAN["scopeId"])
                    plan["rules"] = [
                        plan["rules"][index] for index in case["planRuleIndices"]
                    ]
                    selected = load(runtime_files(plans={plan["scopeId"]: plan}))
                decision = policy.decide_request(selected, facts(case["facts"]))
                expect = case["expect"]
                self.assertEqual(decision.action, expect["action"])
                self.assertEqual(decision.code, expect["code"])
                if expect["action"] == policy.ACTION_INJECT:
                    self.assertEqual(decision.rule, expect["rule"])
                    self.assertEqual(decision.header, expect["header"])
                    self.assertEqual(decision.header_value, expect["headerValue"])
                    self.assertEqual(
                        decision.response_rewrite, expect["responseRewrite"]
                    )
                    presented = next(
                        value
                        for name, value in case["facts"]["headers"]
                        if name.lower() == decision.header.lower()
                    )
                    self.assertEqual(
                        policy.rewrite_bytes(
                            decision.header_value.encode(), decision.replacements
                        ),
                        presented.encode(),
                    )
                    self.assertEqual(
                        policy.rewrite_bytes(
                            VALUES["real"].encode(), decision.replacements
                        ),
                        VALUES["fake"].encode(),
                    )
                else:
                    self.assertIsNone(decision.header)
                    self.assertIsNone(decision.header_value)

    def test_managed_fake_outside_the_injection_header_cannot_forward(self) -> None:
        snapshot = load(runtime_files())
        for placement in (
            "header",
            "header_name",
            "authority",
            "sni",
            "targetAuthority",
            "path",
        ):
            with self.subTest(placement=placement):
                request = copy.deepcopy(API_FACTS)
                request["headers"] = []
                if placement == "header":
                    request["headers"] = [["X-Unconfigured", VALUES["fake"]]]
                elif placement == "header_name":
                    request["headers"] = [[VALUES["fake"], "unmanaged"]]
                else:
                    request[placement] = (
                        "/" + VALUES["fake"] if placement == "path" else VALUES["fake"]
                    )
                decision = policy.decide_request(snapshot, facts(request))
                self.assertEqual(
                    (decision.action, decision.code),
                    ("reject", "credential_not_allowed"),
                )

    def test_unavailable_fake_is_denied_in_every_raw_location_and_explicit_basic(
        self,
    ) -> None:
        snapshot = load(runtime_files())
        for port in (15001, 15002):
            for placement in ("Authorization", "X-Unconfigured", "Host", "basic"):
                with self.subTest(port=port, placement=placement):
                    request = copy.deepcopy(API_FACTS)
                    request["port"] = port
                    fake = VALUES["unavailableFake"]
                    if placement == "basic":
                        encoded = base64.b64encode(f"user:{fake}".encode()).decode()
                        request["headers"] = [["X-Unconfigured", f"Basic {encoded}"]]
                    else:
                        request["headers"] = [[placement, fake]]
                    decision = policy.decide_request(snapshot, facts(request))
                    self.assertEqual(
                        (decision.action, decision.code),
                        ("reject", "credential_not_allowed"),
                    )
                    self.assertIsNone(decision.header_value)
                    self.assertEqual(decision.replacements, ())
        request = copy.deepcopy(API_FACTS)
        request.update(tls=False, headers=[], authority=VALUES["unavailableFake"])
        self.assertEqual(
            policy.decide_request(snapshot, facts(request)).action, "reject"
        )

    def test_unavailable_is_generation_local_and_never_grants_or_permanently_retires(
        self,
    ) -> None:
        unavailable = load(runtime_files())
        request = copy.deepcopy(API_FACTS)
        request["headers"] = [["Authorization", VALUES["unavailableFake"]]]
        secrets = copy.deepcopy(DECISIONS["secrets"])
        identity = secrets["unavailable"].pop()
        real = "R" * len(identity["fake"])
        secrets["entries"].append(
            {
                **identity,
                "real": real,
                "scopeIds": [PLAN["scopeId"]],
            }
        )
        plans = {}
        for listener in DECISIONS["listeners"]:
            plan = plan_for(listener["scopeId"])
            if plan["scopeId"] == PLAN["scopeId"]:
                plan["credentials"].append(identity["credential"])
                rule = copy.deepcopy(plan["rules"][0])
                rule["source"]["ruleIndex"] = 8
                rule["match"]["credentialId"] = identity["credential"]
                plan["rules"].append(rule)
            plans[plan["scopeId"]] = plan
        secrets["generation"] = 2
        active = load(runtime_files(secrets=secrets, plans=plans, generation=2))
        self.assertEqual(
            policy.decide_request(unavailable, facts(request)).action, "reject"
        )
        decision = policy.decide_request(active, facts(request))
        self.assertEqual(
            (decision.action, decision.header_value),
            ("inject", real),
        )
        secrets["entries"].pop()
        secrets["retired"].append(identity)
        secrets["generation"] = 3
        retired = load(runtime_files(secrets=secrets, generation=3))
        self.assertEqual(
            policy.decide_request(retired, facts(request)).code, "retired_credential"
        )
        self.assertEqual(policy.decide_request(active, facts(request)).action, "inject")

    def test_no_snapshot_rejects_everything(self) -> None:
        for case in DECISIONS["cases"]:
            decision = policy.decide_request(None, facts(case["facts"]))
            self.assertEqual(
                (decision.action, decision.code), ("reject", "gateway_not_ready")
            )

    def test_values_never_appear_in_reprs(self) -> None:
        snapshot = load(runtime_files())
        decision = policy.decide_request(
            snapshot, facts(DECISIONS["cases"][0]["facts"])
        )
        text = repr(snapshot) + repr(decision) + repr(list(snapshot.live.values()))
        for value in VALUES.values():
            self.assertNotIn(value, text)


class GenericDecisionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.plan = plan_for(PLAN["scopeId"])
        self.plan["credentials"] = ["service-key"]
        self.plan["rules"] = [self.plan["rules"][0]]
        self.rule = self.plan["rules"][0]
        self.rule.update(header="X-Api-Key", format="auto")
        self.rule["match"].update(host="service.example", credentialId="service-key")
        self.secrets = copy.deepcopy(DECISIONS["secrets"])
        self.secrets["entries"][0]["credential"] = "service-key"
        self.plans = {self.plan["scopeId"]: self.plan}
        foreign = copy.deepcopy(self.plan)
        foreign["scopeId"] = DECISIONS["listeners"][1]["scopeId"]
        foreign["credentials"] = ["pair-other"]
        foreign["rules"][0]["match"]["credentialId"] = "pair-other"
        self.plans[foreign["scopeId"]] = foreign
        self.request = copy.deepcopy(API_FACTS)
        self.request.update(
            sni="service.example",
            authority="service.example",
            path="/v1/items",
            headers=[["x-api-key", VALUES["fake"]]],
        )

    def decide(self) -> policy.Decision:
        return policy.decide_request(
            load(runtime_files(secrets=self.secrets, plans=self.plans)),
            facts(self.request),
        )

    def test_auto_preserves_arbitrary_surrounding_text(self) -> None:
        for presented in (VALUES["fake"], f"Vendor prefix={VALUES['fake']}; suffix"):
            with self.subTest(presented=presented):
                self.request["headers"] = [["x-API-key", presented]]
                decision = self.decide()
                injected = presented.replace(VALUES["fake"], VALUES["real"])
                self.assertEqual(
                    (decision.action, decision.header, decision.header_value),
                    ("inject", "X-Api-Key", injected),
                )
                self.assertEqual(
                    policy.rewrite_bytes(injected.encode(), decision.replacements),
                    presented.encode(),
                )

    def test_template_matches_the_whole_value_and_never_adds_missing_fake(self) -> None:
        self.rule["format"] = "Bearer {fake}"
        for presented, action in (
            (f"Bearer {VALUES['fake']}", "inject"),
            (f"token {VALUES['fake']}", "reject"),
            (f"Bearer {VALUES['fake']} trailing", "reject"),
            (f" Bearer {VALUES['fake']}", "reject"),
            ("Bearer unrelated", "forward"),
        ):
            with self.subTest(presented=presented):
                self.request["headers"] = [["X-Api-Key", presented]]
                self.assertEqual(self.decide().action, action)
        self.request["headers"] = []
        self.assertEqual(self.decide().action, "forward")

    def test_unicode_template_preserves_whole_value_and_response_echo(self) -> None:
        self.rule["format"] = "Vendor cl\u00e9={fake}"
        self.request["headers"] = [["X-Api-Key", f"Vendor cl\u00e9={VALUES['fake']}"]]
        decision = self.decide()
        self.assertEqual(
            (decision.action, decision.header_value),
            ("inject", f"Vendor cl\u00e9={VALUES['real']}"),
        )
        self.assertEqual(
            policy.rewrite_bytes(decision.header_value.encode(), decision.replacements),
            self.request["headers"][0][1].encode(),
        )

    def test_basic_preserves_username_and_redacts_encoded_echoes(self) -> None:
        self.rule["format"] = "basic"
        encoded_fake = base64.b64encode(
            f"synthetic-\u00e9:{VALUES['fake']}".encode()
        ).decode()
        encoded_real = base64.b64encode(
            f"synthetic-\u00e9:{VALUES['real']}".encode()
        ).decode()
        self.request["headers"] = [["X-Api-Key", f"bAsIc {encoded_fake}"]]
        decision = self.decide()
        self.assertEqual(
            (decision.action, decision.header_value),
            ("inject", f"bAsIc {encoded_real}"),
        )
        self.assertEqual(
            policy.rewrite_bytes(
                f"bAsIc {encoded_real}|{encoded_real}|{VALUES['real']}".encode(),
                decision.replacements,
            ),
            f"bAsIc {encoded_fake}|{encoded_fake}|{VALUES['fake']}".encode(),
        )

    def test_known_fake_misuse_and_ambiguity_are_rejected(self) -> None:
        for headers, code in (
            ([["X-Other", VALUES["fake"]]], "credential_not_allowed"),
            ([["X-Api-Key", VALUES["fake"] * 2]], "ambiguous_credential"),
            (
                [["X-Api-Key", VALUES["fake"]], ["X-Other", VALUES["fake"]]],
                "ambiguous_credential",
            ),
            (
                [["X-Api-Key", VALUES["fake"]], ["x-api-key", "unmanaged"]],
                "duplicate_header",
            ),
            (
                [
                    ["X-Api-Key", VALUES["fake"]],
                    ["Host", "service.example"],
                    ["host", "other.example"],
                ],
                "duplicate_header",
            ),
            ([["X-Api-Key", VALUES["retiredFake"]]], "retired_credential"),
            ([["X-Other", VALUES["otherFake"]]], "subject_mismatch"),
        ):
            with self.subTest(headers=headers):
                self.request["headers"] = headers
                decision = self.decide()
                self.assertEqual((decision.action, decision.code), ("reject", code))
        self.secrets["entries"][0].update(real="a" * 40, fake="b" * 40)
        for presented, action, code in (
            ("b" * 40, "inject", "credential_injected"),
            ("b" * 41, "reject", "ambiguous_credential"),
        ):
            with self.subTest(overlapping_length=len(presented)):
                self.request["headers"] = [["X-Api-Key", presented]]
                decision = self.decide()
                self.assertEqual((decision.action, decision.code), (action, code))

    def test_explicit_basic_detects_foreign_and_retired_fakes(self) -> None:
        local = copy.deepcopy(self.plan)
        foreign = copy.deepcopy(self.plans[DECISIONS["listeners"][1]["scopeId"]])
        foreign["rules"][0]["format"] = "basic"
        snapshot = load(
            runtime_files(
                secrets=self.secrets,
                plans={local["scopeId"]: local, foreign["scopeId"]: foreign},
            )
        )
        for password, header, code in (
            (VALUES["otherFake"], "X-Api-Key", "subject_mismatch"),
            (VALUES["retiredFake"], "X-Other", "retired_credential"),
            (VALUES["fake"], "X-Api-Key", "credential_not_allowed"),
            (VALUES["real"], "X-Other", "real_credential_exposed"),
        ):
            with self.subTest(password=password, header=header):
                encoded = base64.b64encode(f"user:{password}".encode()).decode()
                self.request["headers"] = [[header, f"Basic {encoded}"]]
                decision = policy.decide_request(snapshot, facts(self.request))
                self.assertEqual((decision.action, decision.code), ("reject", code))
                # A second unconfigured field must not overwrite detection of the first.
                self.request["headers"].append([header, "Basic dXNlcjp1bm1hbmFnZWQ="])
                decision = policy.decide_request(snapshot, facts(self.request))
                expected = "duplicate_header" if header == "X-Api-Key" else code
                self.assertEqual((decision.action, decision.code), ("reject", expected))

    def test_basic_has_no_implicit_default_or_auto_fallback(self) -> None:
        for presented in (
            "Basic not*base64",
            "Basic " + base64.b64encode(f"user:{VALUES['fake']}".encode()).decode(),
        ):
            self.request["headers"] = [["X-Api-Key", presented]]
            self.assertEqual(self.decide().action, "forward")
        self.rule["format"] = "basic"
        automatic = copy.deepcopy(self.rule)
        automatic["format"] = "auto"
        automatic["source"]["ruleIndex"] = 1
        self.plan["rules"].append(automatic)
        for presented in ("Basic", "Basic not*base64", f"Basic {VALUES['fake']}"):
            with self.subTest(presented=presented):
                self.request["headers"] = [["X-Api-Key", presented]]
                decision = self.decide()
                self.assertEqual(
                    (decision.action, decision.code), ("reject", "malformed_basic")
                )

    def test_basic_requires_an_entire_password_and_never_a_username_fake(self) -> None:
        self.rule["format"] = "basic"
        for username, password, code in (
            ("user", f"prefix{VALUES['fake']}", "credential_not_allowed"),
            (VALUES["fake"], "unmanaged", "fake_in_username"),
            ("user", VALUES["retiredFake"], "retired_credential"),
        ):
            with self.subTest(code=code):
                encoded = base64.b64encode(f"{username}:{password}".encode()).decode()
                self.request["headers"] = [["X-Api-Key", f"Basic {encoded}"]]
                self.assertEqual(self.decide().code, code)

    def test_shared_fake_memberships_and_multiple_local_credentials(self) -> None:
        self.secrets["entries"][0]["scopeIds"].append(
            DECISIONS["listeners"][1]["scopeId"]
        )
        self.secrets["entries"] = self.secrets["entries"][:1]
        foreign = copy.deepcopy(self.plan)
        foreign["scopeId"] = DECISIONS["listeners"][1]["scopeId"]
        self.plans[foreign["scopeId"]] = foreign
        self.request["port"] = 15002
        self.assertEqual(self.decide().action, "inject")
        extra = copy.deepcopy(self.secrets["entries"][0])
        extra.update(fake="other-local-fake", real="R" * 16, credential="other-key")
        self.secrets["entries"].append(extra)
        decision = self.decide()
        self.assertEqual(
            policy.rewrite_bytes(extra["real"].encode(), decision.replacements),
            b"other-local-fake",
        )
        self.request["headers"].append(["X-Other", "other-local-fake"])
        self.assertEqual(self.decide().code, "ambiguous_credential")

    def test_removing_one_shared_scope_does_not_retire_the_other(self) -> None:
        scopes = [listener["scopeId"] for listener in DECISIONS["listeners"]]
        for memberships, codes in (
            (scopes, ["credential_injected", "credential_injected"]),
            (scopes[1:], ["subject_mismatch", "credential_injected"]),
            ([], ["retired_credential", "retired_credential"]),
        ):
            with self.subTest(memberships=memberships):
                secrets = copy.deepcopy(DECISIONS["secrets"])
                shared = secrets["entries"][0]
                shared["scopeIds"] = memberships
                secrets["entries"] = [shared] if memberships else []
                if not memberships:
                    secrets["retired"].append(
                        {"fake": shared["fake"], "credential": shared["credential"]}
                    )
                plans = {}
                for scope in scopes:
                    plan = plan_for(scopes[0])
                    plan["scopeId"] = scope
                    if scope not in memberships:
                        plan.update(credentials=[], rules=[])
                    plans[scope] = plan
                snapshot = load(runtime_files(secrets=secrets, plans=plans))
                request = copy.deepcopy(API_FACTS)
                for listener, code in zip(DECISIONS["listeners"], codes):
                    request["port"] = listener["port"]
                    self.assertEqual(
                        policy.decide_request(snapshot, facts(request)).code, code
                    )

    def test_equivalent_rules_deduplicate_but_formats_and_modes_conflict(self) -> None:
        duplicate = copy.deepcopy(self.rule)
        duplicate["source"]["ruleIndex"] = 1
        duplicate["header"] = "x-api-key"
        duplicate["match"]["path"] = {"kind": "like", "value": "/v1/*"}
        self.plan["rules"].append(duplicate)
        self.assertEqual(self.decide().action, "inject")
        for key, value in (("format", "{fake}"), ("responseRewrite", False)):
            with self.subTest(key=key):
                duplicate[key] = value
                self.assertEqual(self.decide().code, "ambiguous_injection")
                duplicate[key] = self.rule[key]

    def test_all_valid_injections_are_gathered_instead_of_first_match(self) -> None:
        extra = copy.deepcopy(self.secrets["entries"][0])
        extra.update(
            fake="second-fake",
            real="R" * 11,
            credential="second-key",
            scopeIds=[listener["scopeId"] for listener in DECISIONS["listeners"]],
        )
        self.secrets["entries"].append(extra)
        self.plan["credentials"].append("second-key")
        second = copy.deepcopy(self.rule)
        second["source"]["ruleIndex"] = 1
        second["match"]["credentialId"] = "second-key"
        second["header"] = "X-Second-Key"
        self.plan["rules"].append(second)
        self.request["headers"].append(["X-Second-Key", "second-fake"])
        for rules in (self.plan["rules"][:], self.plan["rules"][::-1]):
            self.plan["rules"] = rules
            self.assertEqual(self.decide().code, "ambiguous_credential")

    def test_null_eq_and_like_match_canonical_paths_without_queries(self) -> None:
        for constraint, path, action in (
            (None, "/any/path?key=unchanged", "inject"),
            ({"kind": "eq", "value": "/v1/items"}, "/v1/items?key=unchanged", "inject"),
            ({"kind": "eq", "value": "/v1/items"}, "/v1/items/more", "reject"),
            ({"kind": "like", "value": "/v1/*/items"}, "/v1/a/b/items", "inject"),
            ({"kind": "like", "value": r"/v1/\*"}, "/v1/*", "inject"),
            ({"kind": "like", "value": r"/v1/\*"}, "/v1/anything", "reject"),
            (None, "/v1//items", "reject"),
            (None, "/v1/items#fragment", "reject"),
            (None, "/v1/items%", "reject"),
        ):
            with self.subTest(constraint=constraint, path=path):
                self.rule["match"]["path"] = constraint
                self.request["path"] = path
                self.assertEqual(self.decide().action, action)
                self.assertEqual(self.request["path"], path)

    def test_authority_ports_and_protocol_guards_apply_before_injection(self) -> None:
        for key, value, code in (
            ("authority", "service.example:", "invalid_host"),
            ("authority", "service.example@other.example", "invalid_host"),
            ("authority", "service.example:8443", "credential_not_allowed"),
            ("targetAuthority", "service.example:8443", "authority_mismatch"),
            ("sni", "other.example", "sni_host_mismatch"),
            ("sni", None, "sni_missing"),
            ("tls", False, "plaintext_credential"),
        ):
            with self.subTest(key=key, value=value):
                previous = self.request[key]
                self.request[key] = value
                self.assertEqual(self.decide().code, code)
                self.request[key] = previous
        self.request["headers"].append(["Upgrade", "websocket"])
        self.assertEqual(self.decide().code, "upgrade_not_allowed")


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

    def test_pair_ids_are_unique_across_live_retired_and_unavailable(self) -> None:
        for source in ("entries", "retired", "unavailable"):
            for destination in ("entries", "retired", "unavailable"):
                with self.subTest(source=source, destination=destination):
                    secrets = copy.deepcopy(DECISIONS["secrets"])
                    item = copy.deepcopy(secrets[destination][0])
                    item.update(
                        credential=secrets[source][0]["credential"], fake="F" * 40
                    )
                    if destination == "entries":
                        item["real"] = "R" * 40
                    secrets[destination].append(item)
                    self.assertEqual(
                        self.assert_load_error(runtime_files(secrets=secrets)),
                        "secrets pair ids must be unique",
                    )

    def test_one_pair_cannot_have_cross_scope_aliases_or_conflicting_real_owners(
        self,
    ) -> None:
        for real in (VALUES["real"], "R" * 40):
            with self.subTest(real=real):
                secrets = copy.deepcopy(DECISIONS["secrets"])
                secrets["entries"][1].update(credential="github", real=real)
                foreign = plan_for(PLAN["scopeId"])
                foreign["scopeId"] = DECISIONS["listeners"][1]["scopeId"]
                self.assertEqual(
                    self.assert_load_error(
                        runtime_files(
                            secrets=secrets, plans={foreign["scopeId"]: foreign}
                        )
                    ),
                    "secrets pair ids must be unique",
                )

    def test_loaded_pairs_count_all_positions_with_exact_eighty_percent_threshold(
        self,
    ) -> None:
        plans = {}
        for listener in DECISIONS["listeners"]:
            plan = plan_for(listener["scopeId"])
            plan.update(credentials=[], rules=[])
            plans[listener["scopeId"]] = plan
        for real, fake, valid in (
            ("a", "b", True),
            ("a", "a", False),
            ("aa", "ab", False),
            ("aaaaa", "abbbb", True),
            ("aaaaa", "aabbb", False),
            ("aaaaaa", "aabbbb", False),
            ("aaaaaa", "abbbbb", True),
            ("ab-12", "ab-34", False),
            ("ab-12", "cd-34", True),
            ("abc", "defg", False),
            ("", "", False),
            ("has space", "different", False),
            ("é", "aa", False),
            ("a\n", "bb", False),
            ("aa", "b\t", False),
            ("a" * 512, "b" * 512, True),
            ("a" * 513, "b" * 513, False),
        ):
            with self.subTest(real=real, fake=fake):
                secrets = copy.deepcopy(DECISIONS["secrets"])
                secrets["entries"] = secrets["entries"][:1]
                secrets["entries"][0].update(real=real, fake=fake)
                files = runtime_files(secrets=secrets, plans=plans)
                if valid:
                    self.assertEqual(load(files).live[fake].real, real)
                else:
                    message = self.assert_load_error(files)
                    for value in (real, fake):
                        if value:
                            self.assertNotIn(repr(value), message)

    def test_policy_provenance_must_cite_unique_declared_snapshots(self) -> None:
        for key, value in [
            ("policyId", "policy:unknown"),
            ("policyRevision", 2),
            ("ruleIndex", 1024),
        ]:
            with self.subTest(key=key):
                plan = plan_for(DECISIONS["listeners"][0]["scopeId"])
                plan["rules"][0]["source"][key] = value
                self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))
        plan = plan_for(DECISIONS["listeners"][0]["scopeId"])
        plan["rules"][1]["source"] = copy.deepcopy(plan["rules"][0]["source"])
        self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_plan_scope_must_match_listener(self) -> None:
        self.assert_load_error(
            runtime_files(
                plans={
                    "00000000-0000-4000-8000-000000000001": plan_for(
                        "00000000-0000-4000-8000-000000000002"
                    )
                }
            )
        )

    def test_plan_credential_needs_a_fake_for_its_agent(self) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"] = [
            e
            for e in secrets["entries"]
            if "00000000-0000-4000-8000-000000000002" not in e["scopeIds"]
        ]
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_unknown_fields_are_rejected(self) -> None:
        plan = plan_for("00000000-0000-4000-8000-000000000001")
        plan["rules"][0]["match"]["query"] = "x"
        self.assert_load_error(
            runtime_files(plans={"00000000-0000-4000-8000-000000000001": plan})
        )
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][0]["extra"] = 1
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_invalid_rule_values_are_rejected(self) -> None:
        edits = [
            ("host", "*.github.com"),
            ("path", {"kind": "eq", "value": "/a/../b"}),
            ("path", {"kind": "eq", "value": "/a%2fb"}),
            ("path", "/legacy/**"),
            ("path", {"kind": "glob", "value": "/a/*"}),
            ("path", {"kind": "like", "value": "/a/\\q"}),
            ("path", {"kind": "like", "value": "relative*"}),
            ("host", "127.0.0.1"),
            ("scheme", "http"),
        ]
        for key, value in edits:
            with self.subTest(key=key, value=value):
                plan = plan_for("00000000-0000-4000-8000-000000000001")
                plan["rules"][0]["match"][key] = value
                self.assert_load_error(
                    runtime_files(plans={"00000000-0000-4000-8000-000000000001": plan})
                )
        for key, value in [
            ("methods", []),
            ("methods", ["POST", "GET"]),
            ("methods", ["GET", "GET"]),
            ("methods", ["OPTIONS"]),
            ("credentialId", "other"),
            ("credentialId", "Invalid"),
            ("credentialId", ["github"]),
        ]:
            with self.subTest(key=key, value=value):
                plan = plan_for(PLAN["scopeId"])
                plan["rules"][0]["match"][key] = value
                self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_response_rewrite_requires_a_boolean(self) -> None:
        for value in ("buffered", "none", "true", "false", 0, 1, None, [], {}):
            with self.subTest(value=value):
                plan = plan_for(PLAN["scopeId"])
                plan["rules"][0]["responseRewrite"] = value
                self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))
        plan = plan_for(PLAN["scopeId"])
        del plan["rules"][0]["responseRewrite"]
        self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_unknown_formats_and_protocol_control_headers_are_rejected(self) -> None:
        for key, value in (
            ("format", "raw"),
            ("format", "customize"),
            ("format", "bearer"),
            ("format", "Bearer {real}"),
            ("format", "{fake}:{fake}"),
            ("format", "Bearer {fake}\r\nInjected: x"),
            ("format", 1),
            ("header", "Host"),
            ("header", "CONTENT-LENGTH"),
            ("header", "Content-Type"),
            ("header", "Accept-Encoding"),
            ("header", "Proxy-Authorization"),
            ("header", "Connection"),
            ("header", ":authority"),
            ("header", "X Bad"),
            ("header", "X-Test\n"),
        ):
            with self.subTest(key=key, value=value):
                plan = plan_for(PLAN["scopeId"])
                plan["rules"][0][key] = value
                self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_scope_memberships_are_nonempty_unique_and_one_fake_per_real(self) -> None:
        for scopes in (
            [],
            [PLAN["scopeId"], PLAN["scopeId"]],
            ["Invalid"],
            "not-a-list",
        ):
            with self.subTest(scopes=scopes):
                secrets = copy.deepcopy(DECISIONS["secrets"])
                secrets["entries"][0]["scopeIds"] = scopes
                self.assert_load_error(runtime_files(secrets=secrets))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][1]["scopeIds"].append(PLAN["scopeId"])
        self.assertEqual(
            self.assert_load_error(runtime_files(secrets=secrets)),
            "a scope maps one real value to several fakes",
        )
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"].append(copy.deepcopy(secrets["entries"][0]))
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_invalid_unicode_in_snapshot_cannot_escape_the_load_error_boundary(
        self,
    ) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][0]["scopeIds"] = ["\ud800"]
        self.assert_load_error(runtime_files(secrets=secrets))
        plan = plan_for(PLAN["scopeId"])
        plan["rules"][0]["format"] = "\ud800 {fake}"
        self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_documents_and_requests_enforce_size_limits(self) -> None:
        files = runtime_files()
        files["active.json"] += b" " * policy.MAX_MANIFEST_BYTES
        self.assert_load_error(files)
        request = copy.deepcopy(API_FACTS)
        request["headers"] = [["X-Padding", "x" * policy.MAX_REQUEST_BYTES]]
        decision = policy.decide_request(load(runtime_files()), facts(request))
        self.assertEqual(
            (decision.action, decision.code), ("reject", "request_too_large")
        )

    def test_old_versions_and_unknown_tombstone_fields_are_rejected(self) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["version"] = 1
        self.assert_load_error(runtime_files(secrets=secrets))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["retired"][0]["scopeId"] = PLAN["scopeId"]
        self.assert_load_error(runtime_files(secrets=secrets))
        plan = plan_for(PLAN["scopeId"])
        plan["format"] = "agent-sec-gateway-plan/v1"
        self.assert_load_error(runtime_files(plans={plan["scopeId"]: plan}))

    def test_unavailable_is_required_bounded_and_contains_only_fake_identity(
        self,
    ) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        del secrets["unavailable"]
        self.assert_load_error(runtime_files(secrets=secrets))
        for value in (None, {}, "not-a-list"):
            secrets = copy.deepcopy(DECISIONS["secrets"])
            secrets["unavailable"] = value
            self.assert_load_error(runtime_files(secrets=secrets))
        for key, value in (
            ("real", VALUES["real"]),
            ("scopeIds", [PLAN["scopeId"]]),
            ("fake", ""),
            ("fake", "line\nbreak"),
            ("credential", "Invalid"),
        ):
            with self.subTest(key=key):
                secrets = copy.deepcopy(DECISIONS["secrets"])
                secrets["unavailable"][0][key] = value
                self.assert_load_error(runtime_files(secrets=secrets))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["unavailable"] = [
            {"fake": f"unselected-{index}", "credential": "pending-key"}
            for index in range(policy.MAX_ENTRIES)
        ]
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_live_retired_and_unavailable_fakes_are_unique_and_mutually_exclusive(
        self,
    ) -> None:
        for source in ("entries", "retired", "unavailable"):
            for destination in ("entries", "retired", "unavailable"):
                with self.subTest(source=source, destination=destination):
                    secrets = copy.deepcopy(DECISIONS["secrets"])
                    item = copy.deepcopy(secrets[destination][0])
                    item.update(
                        fake=secrets[source][0]["fake"], credential="pair-duplicate"
                    )
                    if destination == "entries":
                        item["real"] = "R" * len(item["fake"])
                    secrets[destination].append(item)
                    self.assertEqual(
                        self.assert_load_error(runtime_files(secrets=secrets)),
                        "secrets fakes must be unique",
                    )
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["unavailable"][0]["fake"] = VALUES["real"]
        self.assert_load_error(runtime_files(secrets=secrets))

    def test_because_control_characters_are_rejected(self) -> None:
        plan = plan_for("00000000-0000-4000-8000-000000000001")
        plan["rules"][0]["because"] = "line\nbreak"
        self.assert_load_error(
            runtime_files(plans={"00000000-0000-4000-8000-000000000001": plan})
        )

    def test_duplicate_json_keys_are_rejected(self) -> None:
        files = runtime_files()
        text = files["plan-00000000-0000-4000-8000-000000000001.json"].decode()
        files["plan-00000000-0000-4000-8000-000000000001.json"] = text.replace(
            '"scopeId":',
            '"scopeId": "other-scope", "scopeId":',
            1,
        ).encode()
        self.assertIn(
            b'"scopeId": "other-scope"',
            files["plan-00000000-0000-4000-8000-000000000001.json"],
        )
        self.assert_load_error(files)

    def test_duplicate_and_overlapping_values_are_rejected(self) -> None:
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["retired"][0]["fake"] = VALUES["fake"]
        self.assert_load_error(runtime_files(secrets=secrets))
        secrets = copy.deepcopy(DECISIONS["secrets"])
        secrets["entries"][1].update(real=VALUES["fake"], fake="F" * 40)
        self.assertEqual(
            self.assert_load_error(runtime_files(secrets=secrets)),
            "a real value equals a fake",
        )

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
    def test_shared_cedar_eq_and_like_cases_drive_actual_request_decisions(
        self,
    ) -> None:
        for case in DECISIONS["pathCases"]:
            with self.subTest(case=case):
                plan = plan_for(PLAN["scopeId"])
                plan["rules"] = [plan["rules"][0]]
                plan["rules"][0]["match"]["path"] = case["constraint"]
                snapshot = load(runtime_files(plans={plan["scopeId"]: plan}))
                request = copy.deepcopy(API_FACTS)
                request["path"] = case["path"]
                decision = policy.decide_request(snapshot, facts(request))
                self.assertEqual(
                    decision.action, "inject" if case["matches"] else "reject"
                )
                self.assertEqual(request["path"], case["path"])

    def test_github_template_enforces_endpoint_methods_paths_and_response_modes(
        self,
    ) -> None:
        snapshot = load(runtime_files())
        basic = base64.b64encode(f"user:{VALUES['fake']}".encode()).decode()
        for host, path, allowed, mode in (
            ("api.github.com", "/", set(policy.METHODS), True),
            ("api.github.com", "/a/b/c", set(policy.METHODS), True),
            ("github.com", "/g/sub/r.git/info/refs", {"GET"}, True),
            ("github.com", "/o/r.git/git-upload-pack", {"POST"}, True),
            ("github.com", "/o/r.git/git-receive-pack", {"POST"}, True),
            ("github.com", "/info/refs", set(), None),
            ("github.com", "/o/r.git/info/refs/extra", set(), None),
        ):
            for method in (*policy.METHODS, "OPTIONS"):
                for presented in (VALUES["fake"], f"Basic {basic}"):
                    with self.subTest(
                        host=host,
                        path=path,
                        method=method,
                        basic=presented.startswith("Basic"),
                    ):
                        request = copy.deepcopy(API_FACTS)
                        request.update(
                            sni=host,
                            authority=host,
                            path=path,
                            method=method,
                            headers=[["Authorization", presented]],
                        )
                        decision = policy.decide_request(snapshot, facts(request))
                        self.assertEqual(
                            decision.action, "inject" if method in allowed else "reject"
                        )
                        self.assertEqual(
                            decision.response_rewrite,
                            mode if method in allowed else None,
                        )


class RewriteTest(unittest.TestCase):
    def test_rewrite_does_not_replace_its_own_output(self) -> None:
        self.assertEqual(
            policy.rewrite_bytes(
                b"first second", (("first", "second"), ("second", "fake"))
            ),
            b"second fake",
        )

    def test_rewrite_maps_real_to_fake(self) -> None:
        pairs = ((VALUES["real"], VALUES["fake"]),)
        body = f'{{"token":"{VALUES["real"]}","again":"{VALUES["real"]}"}}'.encode()
        rewritten = policy.rewrite_bytes(body, pairs)
        self.assertNotIn(VALUES["real"].encode(), rewritten)
        self.assertEqual(len(rewritten), len(body))
        self.assertEqual(rewritten.count(VALUES["fake"].encode()), 2)


class StreamingRewriteTest(unittest.TestCase):
    def test_partitions_preserve_nonrecursive_longest_original_matches(self) -> None:
        authorization = f"Bearer {VALUES['real']}"
        cases = [
            (
                b"safe ABCDE and ABCDE ABC",
                (("ABCDE", "MASK"),),
                b"safe MASK and MASK ABC",
            ),
            (
                b"aaaaaaabaababab",
                (("aaa", "X"), ("ab", "longer")),
                b"XXlongeralongerlongerlonger",
            ),
            (b"abcabcABCD", (("abc", "AB"), ("ABCD", "safe")), b"ABABsafe"),
            (
                b"xxyxyx",
                (("x", "yz"), ("z", "!")),
                b"yz" * 2 + b"y" + b"yz" + b"y" + b"yz",
            ),
            ("雪ABCDE🌊ABC".encode(), (("ABCDE", "MASK"),), "雪MASK🌊ABC".encode()),
            (
                b"a" * 1024 + b"b",
                (("a" * 512 + "b", "short"),),
                b"a" * 512 + b"short",
            ),
            (
                f"{authorization} / {VALUES['real']} / {authorization}".encode(),
                (
                    (authorization, f"Bearer {VALUES['fake']}"),
                    (VALUES["real"], VALUES["fake"]),
                ),
                (
                    f"Bearer {VALUES['fake']} / {VALUES['fake']} / "
                    f"Bearer {VALUES['fake']}"
                ).encode(),
            ),
            (
                b"abcde abc abcdef",
                (("abc", "SHORT"), ("abcde", "LONG")),
                b"LONG SHORT LONGf",
            ),
            (b"abcde", (("abc", "X"), ("bcde", "Y")), b"Xde"),
            (b"ababa", (("aba", "X"), ("ba", "Y")), b"XY"),
            (
                b"first second third first",
                (("first", "second"), ("second", "third"), ("third", "fake")),
                b"second third fake second",
            ),
            (b"abcabc", (("abc", "abcabc"),), b"abcabcabcabc"),
            (b"A.B A?B", (("A.B", "\\1"), ("A?B", "")), b"\\1 "),
            (b"safe SSE data\n\n", (), b"safe SSE data\n\n"),
        ]
        randomizer = random.Random(0)
        for body, pairs, expected in cases:
            partitions = [
                [body[:split], body[split:]] for split in range(len(body) + 1)
            ]
            partitions.append([body[index : index + 1] for index in range(len(body))])
            for _ in range(10):
                cuts = sorted(
                    {
                        0,
                        len(body),
                        *(randomizer.randrange(len(body)) for _ in range(20)),
                    }
                )
                partitions.append(
                    [body[start:end] for start, end in zip(cuts, cuts[1:])]
                )
            self.assertEqual(policy.rewrite_bytes(body, pairs), expected)
            for chunks in partitions:
                with self.subTest(length=len(body), chunks=len(chunks), pairs=pairs):
                    rewriter = policy.StreamingRewriter(pairs)
                    actual = b"".join(rewriter(chunk) for chunk in chunks if chunk)
                    self.assertEqual(actual + rewriter(b""), expected)

    def test_only_undecided_original_suffixes_are_withheld(self) -> None:
        pairs = (("Bearer ABCDE", "Bearer MASK"), ("ABCDE", "MASK"))
        for replacements, chunks, outputs in [
            (pairs, [b"safe ABC", b"DE world", b""], [b"safe ", b"MASK world", b""]),
            (
                pairs,
                [b"safe Bearer ABC", b"DE world", b""],
                [b"safe ", b"Bearer MASK world", b""],
            ),
            (
                (("abc", "SHORT"), ("abcde", "LONG")),
                [b"safe abc", b"de!", b""],
                [b"safe ", b"LONG!", b""],
            ),
            (
                (("abc", "SHORT"), ("abcde", "LONG")),
                [b"safe abc", b""],
                [b"safe ", b"SHORT"],
            ),
            (
                (("abc", "SHORT"), ("abcde", "LONG")),
                [b"safe abc", b"X", b""],
                [b"safe ", b"SHORTX", b""],
            ),
            (
                (("ABCDE", "LONG"), ("BC", "SHORT")),
                [b"safe ABC", b"DE", b""],
                [b"safe ", b"LONG", b""],
            ),
            (
                (("abc", "X"), ("bcde", "Y")),
                [b"abc", b"de", b""],
                [b"X", b"de", b""],
            ),
            (
                (("a", "b"), ("bc", "Z")),
                [b"a", b"c", b""],
                [b"b", b"c", b""],
            ),
            (
                (("SECRET" * 80, "MASK"),),
                [b"data: ok\n\n", b""],
                [b"data: ok\n\n", b""],
            ),
            ((), [b"data: ok\n\n", b""], [b"data: ok\n\n", b""]),
        ]:
            with self.subTest(replacements=replacements, chunks=chunks):
                rewriter = policy.StreamingRewriter(replacements)
                self.assertEqual([rewriter(chunk) for chunk in chunks], outputs)
                self.assertEqual(rewriter(b""), b"")

    def test_near_match_and_eof_release_only_unmatched_bytes(self) -> None:
        rewriter = policy.StreamingRewriter((("ABABAC", "MASK"),))
        self.assertEqual(rewriter(b"safe ABABA"), b"safe ")
        self.assertEqual(rewriter(b"B"), b"AB")
        self.assertEqual(rewriter(b"AC tail ABA"), b"MASK tail ")
        self.assertEqual(rewriter(b""), b"ABA")

    def test_stream_state_does_not_grow_with_total_body(self) -> None:
        block = b"x" * 65536
        rewriter = policy.StreamingRewriter((("ABCDE", "MASK"),))
        tracemalloc.start()
        try:
            for _ in range(160):
                self.assertEqual(rewriter(block), block)
            self.assertEqual(rewriter(b""), b"")
            _, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertLess(peak, 1024 * 1024)


if __name__ == "__main__":
    unittest.main()
