import json
import sqlite3
import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "assets" / "scripts" / "extract_tool_results.py"


def _create_stats_db(db_path: Path) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(db_path) as conn:
        conn.execute(
            """
            CREATE TABLE stats (
                id INTEGER PRIMARY KEY,
                session_id TEXT,
                tool_use_id TEXT,
                operation TEXT,
                before_text TEXT,
                after_text TEXT,
                content_type TEXT,
                recoverability TEXT,
                applied_operations TEXT,
                before_tokens INTEGER,
                after_tokens INTEGER,
                timestamp_ns INTEGER
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE retrieve_events (
                session_id TEXT,
                timestamp TEXT,
                hash TEXT,
                outcome TEXT,
                tool_use_id TEXT
            )
            """
        )
        conn.execute(
            """
            INSERT INTO stats VALUES (
                1,
                'django__django-12345',
                'toolu_01TestCall',
                'compress-response',
                ?,
                ?,
                'build_log',
                'retrievable',
                '["progress_reduction"]',
                120,
                40,
                1700000001000000000
            )
            """,
            ("original " * 40, "compressed " * 10),
        )


def _create_profile_store(db_path: Path, call_id: str) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(db_path) as conn:
        conn.execute("CREATE TABLE messages (body TEXT)")
        conn.execute(
            "INSERT INTO messages VALUES (?)",
            (json.dumps({"tool_call_id": call_id}),),
        )


def _run_extractor(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        check=False,
    )


def test_stats_db_paths_with_hash_and_percent_match_plain_path(tmp_path: Path) -> None:
    # Keep each variant in its own directory so SQLite decoding '%23' to '#'
    # cannot silently alias the sibling 'stats#notes.sqlite' fixture.
    plain_db = tmp_path / "plain" / "stats-plain.sqlite"
    hash_db = tmp_path / "hash" / "stats#notes.sqlite"
    pct_db = tmp_path / "pct" / "stats%23notes.sqlite"
    for p in (plain_db, hash_db, pct_db):
        _create_stats_db(p)

    plain_out = tmp_path / "plain.json"
    hash_out = tmp_path / "hash.json"
    pct_out = tmp_path / "pct.json"

    for db, out in ((plain_db, plain_out), (hash_db, hash_out), (pct_db, pct_out)):
        res = _run_extractor(
            "--arm",
            "treatment",
            "--since",
            "2023-01-01T00:00:00Z",
            "--stats-db",
            str(db),
            "--output",
            str(out),
        )
        assert res.returncode == 0, res.stderr

    plain_data = json.loads(plain_out.read_text())
    hash_data = json.loads(hash_out.read_text())
    pct_data = json.loads(pct_out.read_text())
    assert hash_data["rows"] == plain_data["rows"]
    assert pct_data["rows"] == plain_data["rows"]
    assert len(hash_data["rows"]) == 1
    # Unescaped '#' previously truncated '?mode=ro' as a URI fragment and caused
    # SQLite to touch an empty 'stats' file beside 'stats#notes.sqlite'.
    assert not (hash_db.parent / "stats").exists()
    assert not (pct_db.parent / "stats").exists()


def test_profiles_dir_sqlite_with_hash_and_percent_preserves_attribution(
    tmp_path: Path,
) -> None:
    proxy_log = tmp_path / "proxy.jsonl"
    call_id = "call_12345"
    entry = {
        "timestamp": "2023-11-15T00:00:00Z",
        "request_messages": [{"role": "tool", "tool_call_id": call_id, "content": "A" * 260}],
        "compressed_messages": [{"role": "tool", "tool_call_id": call_id, "content": "B" * 80}],
    }
    proxy_log.write_text(json.dumps(entry) + "\n")

    plain_profiles = tmp_path / "plain" / "profiles-plain"
    hash_profiles = tmp_path / "hash" / "profiles#notes"
    pct_profiles = tmp_path / "pct" / "profiles%23notes"
    for root in (plain_profiles, hash_profiles, pct_profiles):
        _create_profile_store(root / "django__django-12345" / "profile.sqlite", call_id)

    outputs = {}
    for label, prof in (
        ("plain", plain_profiles),
        ("hash", hash_profiles),
        ("pct", pct_profiles),
    ):
        out = tmp_path / f"{label}-proxy.json"
        res = _run_extractor(
            "--arm",
            "baseline",
            "--since",
            "2023-01-01T00:00:00Z",
            "--proxy-log",
            str(proxy_log),
            "--profiles-dir",
            str(prof),
            "--output",
            str(out),
        )
        assert res.returncode == 0, res.stderr
        outputs[label] = json.loads(out.read_text())

    assert outputs["hash"]["rows"] == outputs["plain"]["rows"]
    assert outputs["pct"]["rows"] == outputs["plain"]["rows"]
    assert outputs["hash"]["rows"][0]["instance"] == "django__django-12345"
    assert not (hash_profiles.parent / "profiles").exists()
    assert not (pct_profiles.parent / "profiles").exists()
