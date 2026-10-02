"""Unit tests for custom PII regex detection."""

import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from agent_sec_cli.pii_checker.custom_rules import (
    CustomPiiRule,
    CustomPiiRuleSet,
    CustomRuleStatus,
)
from agent_sec_cli.pii_checker.detectors import custom as custom_detector_module
from agent_sec_cli.pii_checker.detectors.custom import CustomPiiDetector
from agent_sec_cli.pii_checker.scanner import PiiScanner


def _write_rules(home: Path, content: str) -> Path:
    path = home / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def test_detector_emits_full_match_with_configured_type_and_severity(
    tmp_path: Path,
) -> None:
    path = _write_rules(
        tmp_path,
        """
- type: dogfood_order_no
  regex: 'order=(?P<value>DFT-[A-Z0-9]{8})'
  severity: warn
""".lstrip(),
    )
    detector = CustomPiiDetector(path)

    candidates = detector.detect("before order=DFT-ABC12345 after")

    assert len(candidates) == 1
    candidate = candidates[0]
    assert candidate.pii_type == "dogfood_order_no"
    assert candidate.category == "custom"
    assert candidate.severity == "warn"
    assert candidate.confidence == 1.0
    assert candidate.value == "order=DFT-ABC12345"
    assert candidate.span == (7, 25)
    assert candidate.detector == "custom_rule"
    assert candidate.engine == "regex"
    assert detector.summary()["status"] == "loaded"


def test_inline_flags_and_lookaround_control_the_full_match(tmp_path: Path) -> None:
    path = _write_rules(
        tmp_path,
        """
- type: dogfood_order_no
  regex: '(?i)(?<=order=)dft-[a-z0-9]{8}'
""".lstrip(),
    )
    detector = CustomPiiDetector(path)

    candidates = detector.detect("ORDER=DFT-ABC12345")

    assert len(candidates) == 1
    assert candidates[0].value == "DFT-ABC12345"
    assert candidates[0].span == (6, 18)


def test_runtime_zero_length_matches_are_ignored(tmp_path: Path) -> None:
    path = _write_rules(
        tmp_path,
        "- type: dogfood_boundary\n  regex: '(?=DFT-)'\n",
    )
    detector = CustomPiiDetector(path)

    assert detector.detect("DFT-ABC12345") == []
    assert detector.summary()["runtime_error_count"] == 1


def test_exact_custom_finding_limit_is_not_truncated(tmp_path: Path) -> None:
    path = _write_rules(tmp_path, "- type: dogfood_marker\n  regex: X\n")
    detector = CustomPiiDetector(path)

    candidates = detector.detect("X" * 100)

    assert len(candidates) == 100
    assert detector.summary()["truncated"] is False


def test_custom_finding_limit_sets_truncated_status(tmp_path: Path) -> None:
    path = _write_rules(tmp_path, "- type: dogfood_marker\n  regex: X\n")
    detector = CustomPiiDetector(path)

    candidates = detector.detect("X" * 101)

    assert len(candidates) == 100
    assert detector.summary()["truncated"] is True


def test_deny_rules_run_before_noisy_warn_rules(tmp_path: Path) -> None:
    path = _write_rules(
        tmp_path,
        """
- type: noise_marker
  regex: X
  severity: warn
- type: real_secret
  regex: SECRET
  severity: deny
""".lstrip(),
    )
    detector = CustomPiiDetector(path)

    candidates = detector.detect(("X" * 101) + "SECRET")

    assert len(candidates) == 100
    assert candidates[0].pii_type == "real_secret"
    assert sum(item.pii_type == "noise_marker" for item in candidates) == 99
    assert detector.summary()["truncated"] is True
    assert detector.summary()["budget_exhausted"] is False


def test_rule_order_is_stable_within_each_severity(tmp_path: Path) -> None:
    path = _write_rules(
        tmp_path,
        """
- type: first_warn
  regex: A
  severity: warn
- type: first_deny
  regex: B
  severity: deny
- type: second_warn
  regex: C
  severity: warn
- type: second_deny
  regex: D
  severity: deny
""".lstrip(),
    )
    detector = CustomPiiDetector(path)

    candidates = detector.detect("ABCD")

    assert [item.pii_type for item in candidates] == [
        "first_deny",
        "second_deny",
        "first_warn",
        "second_warn",
    ]


def test_finding_limit_does_not_stop_later_rules(tmp_path: Path) -> None:
    path = _write_rules(
        tmp_path,
        """
- type: noisy_secret
  regex: X
  severity: deny
- type: boundary_probe
  regex: '(?=SECRET)'
  severity: deny
""".lstrip(),
    )
    detector = CustomPiiDetector(path)

    candidates = detector.detect(("X" * 101) + "SECRET")

    assert len(candidates) == 100
    assert detector.summary()["truncated"] is True
    assert detector.summary()["runtime_error_count"] == 1


def test_rule_timeout_is_fail_open_and_sanitized(monkeypatch) -> None:
    class TimeoutPattern:
        def finditer(self, text: str, *, timeout: float):
            raise TimeoutError

    ruleset = CustomPiiRuleSet(
        status=CustomRuleStatus.LOADED,
        rules=(
            CustomPiiRule(
                pii_type="dogfood_token",
                regex="sensitive-pattern",
                severity="deny",
                pattern=TimeoutPattern(),
            ),
        ),
        ruleset_sha256="a" * 64,
    )
    monkeypatch.setattr(
        custom_detector_module, "load_custom_rules", lambda path: ruleset
    )
    detector = CustomPiiDetector()

    assert detector.detect("safe input") == []
    assert detector.summary() == {
        "status": "loaded",
        "rule_count": 1,
        "runtime_error_count": 1,
        "budget_exhausted": False,
        "truncated": False,
        "ruleset_sha256": "a" * 64,
    }


def test_unexpected_rule_error_is_fail_open_and_sanitized(monkeypatch, caplog) -> None:
    class FailingPattern:
        def finditer(self, text: str, *, timeout: float):
            raise ValueError("sensitive internal detail")

    ruleset = CustomPiiRuleSet(
        status=CustomRuleStatus.LOADED,
        rules=(
            CustomPiiRule(
                pii_type="dogfood_token",
                regex="sensitive-pattern",
                severity="deny",
                pattern=FailingPattern(),
            ),
        ),
    )
    monkeypatch.setattr(
        custom_detector_module, "load_custom_rules", lambda path: ruleset
    )
    detector = CustomPiiDetector()

    assert detector.detect("safe input") == []
    assert detector.summary()["runtime_error_count"] == 1
    assert "ValueError" in caplog.text
    assert "sensitive internal detail" not in caplog.text
    assert "sensitive-pattern" not in caplog.text


def test_unexpected_loader_error_is_fail_open_and_sanitized(
    monkeypatch, caplog
) -> None:
    def fail_load(path: Path | None):
        raise ValueError("sensitive loader detail")

    monkeypatch.setattr(custom_detector_module, "load_custom_rules", fail_load)
    detector = CustomPiiDetector()

    assert detector.detect("safe input") == []
    assert detector.summary()["status"] == "invalid"
    assert detector.summary()["runtime_error_count"] == 1
    assert detector.summary()["error_code"] == "load_error"
    assert "ValueError" in caplog.text
    assert "sensitive loader detail" not in caplog.text


def test_shared_scanner_keeps_custom_summary_thread_local(monkeypatch) -> None:
    first_rule_started = threading.Event()
    second_scan_finished = threading.Event()
    scan_context = threading.local()

    class BlockingPattern:
        def finditer(self, text: str, *, timeout: float):
            first_rule_started.set()
            assert second_scan_finished.wait(timeout=2)
            return iter(())

    loaded_ruleset = CustomPiiRuleSet(
        status=CustomRuleStatus.LOADED,
        rules=(
            CustomPiiRule(
                pii_type="thread_local_token",
                regex="token",
                severity="deny",
                pattern=BlockingPattern(),
            ),
        ),
    )
    invalid_ruleset = CustomPiiRuleSet(
        status=CustomRuleStatus.INVALID,
        error_code="invalid_regex",
    )

    def load_rules(path: Path | None):
        if scan_context.name == "first":
            return loaded_ruleset
        assert first_rule_started.wait(timeout=2)
        return invalid_ruleset

    monkeypatch.setattr(custom_detector_module, "load_custom_rules", load_rules)
    scanner = PiiScanner()

    def scan_first():
        scan_context.name = "first"
        return scanner.scan("first")

    def scan_second():
        scan_context.name = "second"
        try:
            return scanner.scan("second")
        finally:
            second_scan_finished.set()

    with ThreadPoolExecutor(max_workers=2) as executor:
        first_future = executor.submit(scan_first)
        second_future = executor.submit(scan_second)
        first_result = first_future.result(timeout=3)
        second_result = second_future.result(timeout=3)

    assert first_result.summary["custom_rules"]["status"] == "loaded"
    assert first_result.summary["custom_rules"]["rule_count"] == 1
    assert second_result.summary["custom_rules"]["status"] == "invalid"
    assert second_result.summary["custom_rules"]["error_code"] == "invalid_regex"


def test_total_budget_stops_before_running_rules(monkeypatch) -> None:
    class UnusedPattern:
        def finditer(self, text: str, *, timeout: float):
            raise AssertionError("rule should not run after budget exhaustion")

    ruleset = CustomPiiRuleSet(
        status=CustomRuleStatus.LOADED,
        rules=(
            CustomPiiRule(
                pii_type="dogfood_token",
                regex="unused",
                severity="deny",
                pattern=UnusedPattern(),
            ),
        ),
    )
    times = iter((0.0, 1.0))
    monkeypatch.setattr(
        custom_detector_module, "load_custom_rules", lambda path: ruleset
    )
    monkeypatch.setattr(
        custom_detector_module.time, "perf_counter", lambda: next(times)
    )
    detector = CustomPiiDetector()

    assert detector.detect("safe input") == []
    assert detector.summary()["budget_exhausted"] is True


def test_default_scanner_reloads_fixed_file_and_keeps_builtins_on_error(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    path = _write_rules(
        tmp_path,
        "- type: dogfood_token\n  regex: 'DFT-[A-Z0-9]{8}'\n",
    )
    scanner = PiiScanner()

    loaded = scanner.scan("DFT-ABC12345", redact_output=True).to_dict()
    path.write_text("- type: dogfood_token\n  regex: '[invalid'\n", encoding="utf-8")
    invalid = scanner.scan("alice@company.cn DFT-ABC12345").to_dict()

    assert loaded["verdict"] == "deny"
    assert loaded["findings"][0]["type"] == "dogfood_token"
    assert loaded["findings"][0]["evidence_redacted"] == "[DOGFOOD_TOKEN_REDACTED]"
    assert loaded["redacted_text"] == "[DOGFOOD_TOKEN_REDACTED]"
    assert loaded["summary"]["custom_rules"]["status"] == "loaded"
    assert invalid["verdict"] == "warn"
    assert {finding["type"] for finding in invalid["findings"]} == {"email"}
    assert invalid["summary"]["custom_rules"]["status"] == "invalid"


def test_same_span_different_custom_types_are_preserved_and_redacted_once(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    _write_rules(
        tmp_path,
        """
- type: alpha_token
  regex: 'DFT-[A-Z0-9]{8}'
- type: beta_token
  regex: 'DFT-[A-Z0-9]{8}'
""".lstrip(),
    )

    result = PiiScanner().scan("DFT-ABC12345", redact_output=True).to_dict()

    assert {finding["type"] for finding in result["findings"]} == {
        "alpha_token",
        "beta_token",
    }
    assert result["summary"]["total"] == 2
    assert result["redacted_text"] == "[ALPHA_TOKEN_REDACTED]"


def test_short_custom_match_preserves_long_builtin_span_and_redacts_union(
    monkeypatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    _write_rules(
        tmp_path,
        "- type: business_prefix\n  regex: 'LTAIabcd'\n  severity: warn\n",
    )
    sensitive = "LTAIabcdefghijklmnop"

    result = PiiScanner().scan(sensitive, redact_output=True).to_dict()

    assert {finding["type"] for finding in result["findings"]} == {
        "aliyun_access_key_id",
        "business_prefix",
    }
    assert result["verdict"] == "deny"
    assert result["redacted_text"] == "[BUSINESS_PREFIX_REDACTED]"
    assert sensitive not in result["redacted_text"]
