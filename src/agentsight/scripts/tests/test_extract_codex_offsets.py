"""Contract tests for the codex OpenSSL offset extractor.

extract-codex-offsets.py builds the Tier 3 offset table entry that the
Rust user-space gate routes BPF probes by. The load-bearing choices are
the _ex symbol preference (the gate's write_is_ex/read_is_ex flags), the
nm parsing rules, and the 64 KiB head fingerprint — a drift in any of
them mismatches the entry to the binary it claims to describe.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
from pathlib import Path
from typing import Optional

SCRIPT = Path(__file__).parents[1] / "extract-codex-offsets.py"

spec = importlib.util.spec_from_file_location("extract_codex_offsets", SCRIPT)
codex = importlib.util.module_from_spec(spec)
sys.modules["extract_codex_offsets"] = codex
spec.loader.exec_module(codex)

EX_OFFSETS = {
    "ssl_write": 0x10,
    "ssl_read": 0x20,
    "ssl_do_handshake": 0x30,
    "write_is_ex": True,
    "read_is_ex": True,
}


def syms_ex() -> dict[str, int]:
    return {"SSL_write_ex": 0x10, "SSL_read_ex": 0x20, "SSL_do_handshake": 0x30}


def syms_plain() -> dict[str, int]:
    return {"SSL_write": 0x11, "SSL_read": 0x21, "SSL_do_handshake": 0x31}


# ─── pick_offsets: the _ex preference ─────────────────────────────────────────


def test_the_ex_variants_are_preferred_when_all_three_are_present() -> None:
    assert codex.pick_offsets(syms_ex()) == EX_OFFSETS


def test_the_ex_variants_win_when_both_sets_are_present() -> None:
    # a binary exporting both generations (e.g. compat shims) must route the
    # gate to the _ex entry points: they report byte counts instead of 0/1
    both = {**syms_plain(), **syms_ex()}
    offsets = codex.pick_offsets(both)
    assert offsets == EX_OFFSETS


def test_the_plain_variants_are_the_fallback() -> None:
    offsets = codex.pick_offsets(syms_plain())
    assert offsets == {
        "ssl_write": 0x11,
        "ssl_read": 0x21,
        "ssl_do_handshake": 0x31,
        "write_is_ex": False,
        "read_is_ex": False,
    }


def test_a_partial_ex_set_does_not_qualify_and_falls_back_to_plain() -> None:
    partial = syms_plain()
    partial["SSL_write_ex"] = 0x99  # only one _ex variant: not enough
    offsets = codex.pick_offsets(partial)
    assert offsets is not None
    assert offsets["ssl_write"] == 0x11
    assert offsets["write_is_ex"] is False


def test_neither_set_complete_yields_no_entry() -> None:
    assert codex.pick_offsets({"SSL_write": 0x1, "malloc": 0x2}) is None


# ─── nm_symbols: parsing rules ────────────────────────────────────────────────


def fake_nm(monkeypatch, outputs: dict[tuple[str, ...], str]) -> list[list[str]]:
    calls: list[list[str]] = []

    def check_output(args, **_kwargs):
        calls.append(args)
        key = tuple(args)
        if key not in outputs:
            raise subprocess.CalledProcessError(1, args)
        return outputs[key]

    monkeypatch.setattr(subprocess, "check_output", check_output)
    return calls


def test_nm_symbols_keep_only_defined_text_symbols(monkeypatch) -> None:
    fake_nm(monkeypatch, {
        ("nm", "--defined-only", "bin"): "\n".join([
            "0000000000000010 T SSL_write_ex",
            "0000000000000020 t SSL_read_ex",      # local text counts too
            "0000000000000030 W SSL_do_handshake", # weak text counts
            "0000000000000040 w some_weak_fn",
            "0000000000000050 U undefined_ref",    # undefined: skipped
            "0000000000000060 D some_data",        # data: skipped
            "0000000000000070 B some_bss",         # bss: skipped
            "garbage-line",
            "0000000000000080 T",                  # no symbol name: skipped
        ]),
    })

    syms = codex.nm_symbols("bin")

    assert syms == {
        "SSL_write_ex": 0x10,
        "SSL_read_ex": 0x20,
        "SSL_do_handshake": 0x30,
        "some_weak_fn": 0x40,
    }


def test_the_regular_table_wins_on_collisions(monkeypatch) -> None:
    fake_nm(monkeypatch, {
        ("nm", "--defined-only", "bin"): "0000000000000010 T SSL_write_ex",
        ("nm", "-D", "--defined-only", "bin"): "0000000000000099 T SSL_write_ex",
    })

    syms = codex.nm_symbols("bin")

    assert syms["SSL_write_ex"] == 0x10


def test_a_stripped_symtab_falls_back_to_the_dynamic_table(monkeypatch) -> None:
    calls = fake_nm(monkeypatch, {
        ("nm", "-D", "--defined-only", "bin"): "0000000000000010 T SSL_write_ex",
    })

    syms = codex.nm_symbols("bin")

    assert syms == {"SSL_write_ex": 0x10}
    assert len(calls) == 2  # regular first, then -D


# ─── the file fingerprint pieces ─────────────────────────────────────────────


def test_sha256_head_hashes_only_the_first_64k(tmp_path: Path) -> None:
    blob = tmp_path / "big.bin"
    payload = bytes(range(256)) * 1024  # 256 KiB
    blob.write_bytes(payload)

    expected = hashlib.sha256(payload[: codex.HEAD_SIZE]).hexdigest()
    assert codex.sha256_head(str(blob)) == expected
    # and it is NOT the hash of the whole file
    assert expected != hashlib.sha256(payload).hexdigest()


def test_detect_codex_version_reads_both_embedded_shapes(tmp_path: Path) -> None:
    cli_style = tmp_path / "a.bin"
    cli_style.write_bytes(b"junk" * 10 + b"codex-cli 0.9.2 junk")
    assert codex.detect_codex_version(str(cli_style)) == "0.9.2"

    rust_style = tmp_path / "b.bin"
    rust_style.write_bytes(b"rust-v1.2.3 junk")
    assert codex.detect_codex_version(str(rust_style)) == "1.2.3"

    nothing = tmp_path / "c.bin"
    nothing.write_bytes(b"no version here")
    assert codex.detect_codex_version(str(nothing)) is None


def test_read_buildid_parses_the_note_line(monkeypatch) -> None:
    fake_nm(monkeypatch, {
        ("readelf", "-n", "bin"): (
            "Displaying notes found in: file\n"
            "  Owner                 Data size Description\n"
            "  GNU                  0x00000014 NT_GNU_BUILD_ID\n"
            "    Build ID: 4fc749c1c0f7a1b2\n"
        ),
    })
    assert codex.read_buildid("bin") == "4fc749c1c0f7a1b2"


def test_read_buildid_returns_none_when_readelf_is_missing(monkeypatch) -> None:
    def boom(*_args, **_kwargs):
        raise FileNotFoundError("readelf")

    monkeypatch.setattr(subprocess, "check_output", boom)
    assert codex.read_buildid("bin") is None


# ─── main: the four outcomes ──────────────────────────────────────────────────


def run_main(monkeypatch, tmp_path: Path, *, binary: str, syms: Optional[dict],
             buildid: Optional[str], capsys) -> tuple[int, str]:
    monkeypatch.setattr(sys, "argv", ["extract-codex-offsets.py", binary])
    monkeypatch.setattr(codex, "nm_symbols", lambda _b: syms or {})
    monkeypatch.setattr(codex, "read_buildid", lambda _b: buildid)
    code = codex.main()
    captured = capsys.readouterr()
    return code, captured.out + captured.err


def test_main_prints_the_full_entry_for_an_ex_binary(monkeypatch, tmp_path, capsys) -> None:
    binary = tmp_path / "codex.bin"
    payload = b"codex-cli 0.9.2 " + b"z" * 200_000  # > 64 KiB head
    binary.write_bytes(payload)

    code, out = run_main(
        monkeypatch, tmp_path, binary=str(binary), syms=syms_ex(),
        buildid="abcd1234", capsys=capsys,
    )

    assert code == 0
    entry = json.loads(out)
    assert entry["codex_version"] == "0.9.2"
    assert entry["offsets"] == EX_OFFSETS
    assert entry["fingerprint"]["build_id"] == "abcd1234"
    assert entry["fingerprint"]["file_size"] == len(payload)
    assert entry["fingerprint"]["head_64k_sha256"] == hashlib.sha256(
        payload[: codex.HEAD_SIZE]
    ).hexdigest()


def test_main_without_a_build_id_omits_the_field(monkeypatch, tmp_path, capsys) -> None:
    binary = tmp_path / "codex.bin"
    binary.write_bytes(b"rust-v2.0.0")

    code, out = run_main(
        monkeypatch, tmp_path, binary=str(binary), syms=syms_plain(),
        buildid=None, capsys=capsys,
    )

    assert code == 0
    entry = json.loads(out)
    assert "build_id" not in entry["fingerprint"]
    assert entry["offsets"]["write_is_ex"] is False


def test_main_rejects_a_missing_file(monkeypatch, tmp_path, capsys) -> None:
    code, out = run_main(
        monkeypatch, tmp_path, binary=str(tmp_path / "nope"),
        syms=None, buildid=None, capsys=capsys,
    )
    assert code == 1
    assert "not a file" in out


def test_main_rejects_a_stripped_binary(monkeypatch, tmp_path, capsys) -> None:
    binary = tmp_path / "stripped.bin"
    binary.write_bytes(b"x")
    code, out = run_main(
        monkeypatch, tmp_path, binary=str(binary), syms=None, buildid=None, capsys=capsys,
    )
    assert code == 2
    assert "no symbols" in out


def test_main_names_the_missing_ssl_symbols(monkeypatch, tmp_path, capsys) -> None:
    binary = tmp_path / "foreign.bin"
    binary.write_bytes(b"x")
    code, out = run_main(
        monkeypatch, tmp_path, binary=str(binary),
        syms={"SSL_write": 0x1},  # neither set complete
        buildid=None, capsys=capsys,
    )
    assert code == 3
    assert "SSL_read" in out
    assert "SSL_write_ex" in out
