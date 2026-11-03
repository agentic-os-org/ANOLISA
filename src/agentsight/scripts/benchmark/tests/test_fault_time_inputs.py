"""Invalid timing controls must fail before network or artifact side effects."""

import sys
from pathlib import Path

import pytest
from test_benchmark import fault_injector, mock_llm_server


class ReachedNetwork(Exception):
    """Stop a valid command at the first observable network boundary."""


@pytest.mark.parametrize("value", ["nan", "inf", "-inf", "1e999", "0", "-1"])
def test_fault_timeout_rejected_before_injection(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    output = tmp_path / "fault.json"
    output.write_text("previous evidence")

    def unexpected(*args: object) -> dict[str, object]:
        pytest.fail("invalid timeout reached network injection")

    monkeypatch.setattr(fault_injector, "inject", unexpected)
    monkeypatch.setattr(
        sys, "argv", ["fault_injector.py", "--timeout=" + value, "--output", str(output)]
    )
    with pytest.raises(SystemExit):
        fault_injector.main()
    assert output.read_text() == "previous evidence"


@pytest.mark.parametrize(
    ("option", "value"),
    [
        ("chunk-delay-ms", "nan"),
        ("chunk-delay-ms", "inf"),
        ("chunk-delay-ms", "-inf"),
        ("chunk-delay-ms", "1e999"),
        ("chunk-delay-ms", "-1"),
        ("tokens-per-second", "nan"),
        ("tokens-per-second", "inf"),
        ("tokens-per-second", "0"),
        ("tokens-per-second", "-1"),
    ],
)
def test_mock_timing_rejected_before_ledger_or_bind(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    option: str,
    value: str,
    capsys: pytest.CaptureFixture[str],
) -> None:
    ledger = tmp_path / "ledger"

    def unexpected(*args: object) -> None:
        pytest.fail("invalid timing reached recorder creation")

    monkeypatch.setattr(mock_llm_server, "RequestRecorder", unexpected)
    args = [
        "mock_llm_server.py",
        "--cert",
        "absent.pem",
        "--key",
        "absent.key",
        "--request-log-dir",
        str(ledger),
        "--" + option + "=" + value,
    ]
    if option == "tokens-per-second":
        args += ["--output-tokens", "10"]
    monkeypatch.setattr(sys, "argv", args)
    with pytest.raises(SystemExit):
        mock_llm_server.main()
    assert not ledger.exists()
    assert "Traceback" not in capsys.readouterr().err


@pytest.mark.parametrize("value", ["0.001", "2"])
def test_positive_fault_timeout_keeps_existing_entrypoint(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    def boundary(*args: object) -> dict[str, object]:
        raise ReachedNetwork

    monkeypatch.setattr(fault_injector, "inject", boundary)
    monkeypatch.setattr(
        sys, "argv", ["fault_injector.py", "--timeout", value, "--output", str(tmp_path / "out")]
    )
    with pytest.raises(ReachedNetwork):
        fault_injector.main()


@pytest.mark.parametrize("value", ["0", "0.001", "2"])
def test_finite_mock_delay_keeps_zero_and_positive_values(
    monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    def boundary(*args: object) -> None:
        raise ReachedNetwork

    monkeypatch.setattr(mock_llm_server, "RequestRecorder", boundary)
    monkeypatch.setattr(
        sys,
        "argv",
        ["mock_llm_server.py", "--cert", "absent", "--key", "absent", "--chunk-delay-ms", value],
    )
    with pytest.raises(ReachedNetwork):
        mock_llm_server.main()
