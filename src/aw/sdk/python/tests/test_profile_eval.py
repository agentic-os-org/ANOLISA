"""Check the Python profile contract against the shared Rust vectors."""

from __future__ import annotations

import inspect
import json
import unittest
from copy import deepcopy
from pathlib import Path
from typing import Any

from aw_sdk import AwError
from aw_sdk._profile_eval import (
    CallFields,
    compile_hooks,
    parse_profile,
    version_matches,
)


def target(
    module: str = "profile_demo",
    qualname: str = "run",
    *,
    call: str = "sync",
    hooks: dict[str, Any] | None = None,
    fields: dict[str, Any] | None = None,
    **extra: Any,
) -> dict[str, Any]:
    return {
        "id": qualname,
        "module": module,
        "qualname": qualname,
        "call": call,
        "hooks": {"session.start": capability()} if hooks is None else hooks,
        "fields": {} if fields is None else fields,
        **extra,
    }


def capability(
    *effects: str, actions: tuple[str, ...] = (), mode: str = "observe"
) -> dict:
    return {
        "mode": mode,
        "effects": ["observe", *effects],
        "failure_actions": list(actions),
    }


def document(*targets: dict[str, Any], origin: str = "builtin") -> dict[str, Any]:
    return {
        "version": "aw-instrumentation-profile/v1alpha1",
        "id": "test.python-profile",
        "origin": origin,
        "runtime": "python",
        "package": {"name": "profile-demo", "versions": ">=1.2,<2"},
        "targets": list(targets),
    }


def field(path: str, codec: str = "text") -> dict[str, str]:
    return {"from": path, "codec": codec}


class ProfileContractTests(unittest.TestCase):
    def test_shared_contract_vectors(self) -> None:
        vectors = (
            Path(__file__).resolve().parents[3]
            / "crates/aw-contracts/tests/fixtures/instrumentation-profile-vectors.json"
        )
        fixture = json.loads(vectors.read_bytes())
        for vector in fixture["valid"]:
            with self.subTest(vector["name"]):
                if vector["document"]["runtime"] == "python":
                    self.assertEqual(
                        parse_profile(vector["document"]), vector["document"]
                    )
                else:
                    with self.assertRaisesRegex(AwError, "source_profile_invalid"):
                        parse_profile(vector["document"])
        for vector in fixture["invalid"]:
            with (
                self.subTest(vector["name"]),
                self.assertRaisesRegex(AwError, "source_profile_invalid"),
            ):
                parse_profile(vector["document"])

    def test_json_limits_and_unknown_fields(self) -> None:
        data = document(target())
        encoded = json.dumps(data).encode()
        self.assertEqual(parse_profile(encoded), data)
        for bad in (
            encoded.replace(
                b'"runtime": "python"', b'"runtime": "js", "runtime": "python"'
            ),
            encoded.replace(b'"fields": {}', b'"fields": {"x": 1, "x": 2}'),
            encoded.replace(b'"fields": {}', b'"fields": {}, "extra": true'),
            encoded.replace(b'"fields": {}', b'"fields": {}, "stream": null'),
            encoded + b" " * (1024 * 1024),
            encoded.replace(b'"test.python-profile"', b'"\\ud800"'),
            b"\xff",
        ):
            with (
                self.subTest(bad=bad[:60]),
                self.assertRaisesRegex(AwError, "source_profile_invalid"),
            ):
                parse_profile(bad)

    def test_conservative_hook_capability_intersection(self) -> None:
        fields = {"tool_name": {"const": "run"}, "input": field("arg.0", "json")}
        one = target(
            qualname="one",
            hooks={
                "tool.before": capability(
                    "block",
                    "replace_input",
                    actions=("report", "block"),
                    mode="control",
                )
            },
            fields=fields,
        )
        two = target(
            qualname="two",
            hooks={
                "tool.before": capability("block", actions=("block",), mode="control")
            },
            fields=fields,
        )
        targets = parse_profile(document(one, two))["targets"]
        self.assertEqual(
            compile_hooks(targets),
            {"tool.before": capability("block", actions=("block",), mode="control")},
        )
        two["hooks"]["tool.before"] = capability(actions=("report",))
        self.assertEqual(
            compile_hooks(parse_profile(document(one, two))["targets"]),
            {"tool.before": capability(actions=("report",))},
        )
        invalid = deepcopy(targets)
        invalid[0]["hooks"]["tool.before"]["effects"].remove("observe")
        with self.assertRaisesRegex(AwError, "source_profile_invalid"):
            compile_hooks(invalid)

    def test_numeric_release_matching(self) -> None:
        for installed, comparators, matches in (
            ("1.10.0", ">=1.9,<2", True),
            ("v01.2.00rc2.post1+build.01", "==1.2", True),
            ("0!1.2-3", ">1.1,<=1.2.0.0", True),
            ("1.2.0.0.0", "==1.2", True),
            ("1.2.0.dev2", ">1.2", False),
            ("1.2.0.0.1", "<=1.2", False),
            ("1!1.2", "==1.2", False),
            ("1.2-garbage", ">=1.2", False),
        ):
            with self.subTest(installed, comparators=comparators):
                self.assertIs(version_matches(installed, comparators), matches)


class ProfileFieldsTests(unittest.TestCase):
    def test_binding_excludes_receiver_and_matches_named_parameters(self) -> None:
        class Agent:
            __slots__ = ("identity",)

            def run(
                self,
                first: str,
                data: dict[str, Any],
                /,
                mode: str = "default",
                *extra: str,
                **options: Any,
            ) -> None:
                pass

        instance = Agent()
        instance.identity = "session-1"
        value = {"payload": [{"command": "original"}]}
        call = CallFields(
            inspect.signature(Agent.run),
            True,
            (instance, "tool", value, "mode", "extra"),
            {"sid": "session-2"},
        )
        self.assertEqual(call.read(field("receiver.identity")), "session-1")
        self.assertEqual(call.read(field("arg.0")), "tool")
        self.assertEqual(call.read(field("arg.3")), "extra")
        self.assertEqual(call.read(field("kwarg.mode")), "mode")
        self.assertEqual(call.read(field("kwarg.sid")), "session-2")
        slot = field("arg.1.payload.0", "json")
        snapshot = call.read(slot)
        snapshot["command"] = "modified snapshot"
        call.write_input(slot, {"command": "approved"})
        self.assertEqual(call.read(slot), {"command": "approved"})
        self.assertEqual(value, {"payload": [{"command": "original"}]})

    def test_stored_attributes_are_readable_but_never_copied_for_input(self) -> None:
        class Envelope:
            def __copy__(self):
                self.fail_copy = True
                return self

        envelope = Envelope()
        envelope.payload = {"key": "original"}
        call = CallFields(None, False, (envelope,), {})
        binding = field("arg.0.payload", "json")
        self.assertEqual(call.read(binding), envelope.payload)
        with self.assertRaisesRegex(AwError, "source_profile_field_write_unsupported"):
            call.isolate_input(binding)
        self.assertFalse(hasattr(envelope, "fail_copy"))
        self.assertIs(call.args[0], envelope)

    def test_signature_and_raw_argument_forms(self) -> None:
        def invoke(payload: str, *, turn: str | None = None) -> None:
            pass

        signature = inspect.signature(invoke)
        binding = field("arg.0", "json_string")
        for call in (
            CallFields(signature, False, (), {"payload": '{"a":1}'}),
            CallFields(signature, False, ('{"a":1}',), {}),
            CallFields(None, False, ('{"a":1}',), {}),
        ):
            self.assertEqual(call.read(binding), {"a": 1})
            call.write_input(binding, {"a": "你好"})
            self.assertEqual(call.read(binding), {"a": "你好"})
            self.assertIn("你好", call.args[0] if call.args else call.kwargs["payload"])
        keyword = CallFields(signature, False, (), {"payload": '{"old":true}'})
        keyword.write_input(binding, {"new": True})
        self.assertEqual(keyword.args, ())
        self.assertEqual(json.loads(keyword.kwargs["payload"]), {"new": True})
        positional = CallFields(signature, False, ("text",), {})
        self.assertEqual(positional.read(field("kwarg.payload")), "text")
        self.assertIsNone(positional.read(field("kwarg.turn"), nullable=True))
        raw = CallFields(None, False, (), {"name": "tool"})
        self.assertEqual(raw.read(field("kwarg.name")), "tool")

    def test_result_codec_and_path_read_errors(self) -> None:
        class Record:
            @property
            def property_value(self) -> str:
                raise AssertionError("field path invoked a property")

        fields = CallFields(None, False, (Record(),), {})
        with self.assertRaisesRegex(AwError, "source_profile_field_missing"):
            fields.read(field("arg.0.property_value"))
        for value in ('{"a":NaN}', '{"a":1,"a":2}'):
            with self.assertRaisesRegex(AwError, "source_invalid_payload"):
                fields.read(field("result", "json_string"), value)
        with self.assertRaisesRegex(AwError, "source_profile_text_required"):
            fields.read(field("result"), None)
        with self.assertRaisesRegex(AwError, "source_invalid_payload"):
            fields.read(field("result", "json"), object())
        original = {"nested": ('{"a":1}',)}
        slot = field("result.nested.0", "json_string")
        self.assertEqual(fields.read(slot, original), {"a": 1})
        self.assertEqual(fields.write_result(slot, original, [1]), {"nested": ("[1]",)})
        self.assertEqual(original, {"nested": ('{"a":1}',)})
