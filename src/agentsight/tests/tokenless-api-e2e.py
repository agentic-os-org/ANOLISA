#!/usr/bin/env python3
"""Verify real Tokenless and AgentSight processes with isolated databases."""
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import socket
import shutil
import sqlite3
import subprocess
import tempfile
import time
from typing import Any
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
TOKENLESS = ROOT.parent / "tokenless/target/debug/tokenless"
AGENTSIGHT = ROOT / "target/debug/agentsight"


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class TokenlessRecordingTest(unittest.TestCase):
    def test_record_content_modes_association_restart_and_failures(self) -> None:
        with tempfile.TemporaryDirectory(prefix="agentsight-stats-e2e-") as temporary:
            directory = Path(temporary)
            producer, observer = directory / "producer", directory / "observer"
            producer.mkdir()
            observer.mkdir()
            environment = {**os.environ, "TOKENLESS_DATA_DIR": str(producer),
                           "TOKENLESS_STATS_ENABLED": "1", "TOKENLESS_SLS_ENABLED": "0",
                           "TOKENLESS_COMPRESSION_ENABLED": "1"}
            subprocess.run([str(TOKENLESS), "compress-response", "--agent-id", "fixture",
                            "--session-id", "source-session", "--tool-use-id", "fixture-tool",
                            "--no-stash", "--truncate-strings-at", "64"],
                           input=json.dumps({"output": "fixture output " * 300}), text=True,
                           env=environment, capture_output=True, check=True)
            subprocess.run([str(TOKENLESS), "compress-response", "--no-stash", "--truncate-strings-at", "64"],
                           input=json.dumps({"output": "unassigned fixture " * 300}), text=True,
                           env=environment, capture_output=True, check=True)
            with sqlite3.connect(producer / "stats.db") as connection:
                connection.row_factory = sqlite3.Row
                original = dict(connection.execute("SELECT * FROM stats WHERE tool_use_id IS NOT NULL").fetchone())
                columns = [key for key in original if key != "id"]
                for mode in ["dry-run", None]:
                    row = {**original, "mode": mode}
                    connection.execute(f"INSERT INTO stats ({','.join(columns)}) VALUES ({','.join('?' for _ in columns)})",
                                       [row[key] for key in columns])
            credential = directory / "credential"
            service_token = secrets.token_urlsafe(32)
            credential.write_text(service_token + "\n")
            credential.chmod(0o600)
            source_port, sight_port = free_port(), free_port()
            source_url, sight_url = f"http://127.0.0.1:{source_port}", f"http://127.0.0.1:{sight_port}"
            config = directory / "config.json"
            configuration = json.loads((ROOT / "agentsight.json").read_text())
            configuration["storage"]["base_path"] = str(observer)
            configuration["tokenless_api"] = {"url": source_url, "credential_file": str(credential)}
            config.write_text(json.dumps(configuration))
            local_home = directory / "home"
            (local_home / ".tokenless").mkdir(parents=True)
            shutil.copy2(producer / "stats.db", local_home / ".tokenless/stats.db")
            logs: list[Any] = []
            processes: list[subprocess.Popen[Any]] = []

            def launch(arguments: list[str]) -> subprocess.Popen[Any]:
                log = tempfile.TemporaryFile()
                logs.append(log)
                process = subprocess.Popen(arguments, stdout=log, stderr=log, env={**os.environ,"HOME":str(local_home)})
                processes.append(process)
                return process

            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

            def query(path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
                request = urllib.request.Request(sight_url + path,
                    data=json.dumps(body).encode() if body is not None else None,
                    headers={"Content-Type": "application/json"})
                try:
                    with opener.open(request, timeout=10) as response:
                        return json.load(response)
                except urllib.error.HTTPError as error:
                    error.add_note(error.read().decode())
                    raise

            def ready(process: subprocess.Popen[Any], path: str) -> None:
                for _ in range(100):
                    if process.poll() is not None:
                        self.fail(f"service exited with {process.returncode}")
                    try:
                        urllib.request.urlopen(path, timeout=0.2).close()
                        return
                    except urllib.error.HTTPError:
                        return
                    except (urllib.error.URLError, TimeoutError):
                        time.sleep(0.05)
                self.fail("service did not become ready")

            def sight() -> subprocess.Popen[Any]:
                process = launch([str(AGENTSIGHT), "serve", "--config", str(config),
                                  "--db", str(observer / "genai_events.db"), "--port", str(sight_port)])
                ready(process, sight_url + "/api/auth/status")
                query("/api/auth/login", {"token": (observer / ".dashboard_token").read_text().strip()})
                return process

            try:
                source = launch([str(TOKENLESS), "stats", "serve", "--data-dir", str(producer),
                                 "--credential-file", str(credential), "--listen", f"127.0.0.1:{source_port}"])
                ready(source, source_url + "/v1/stats/records")
                observer_process = sight()
                self.assertIn("token_savings", query("/api/auth/status")["capabilities"])
                unauthenticated = urllib.request.Request(sight_url + "/api/tokenless/refresh",
                    data=b'{"session_id":"source-session"}', headers={"Content-Type": "application/json"})
                with self.assertRaises(urllib.error.HTTPError) as error:
                    urllib.request.urlopen(unauthenticated)
                self.assertEqual(error.exception.code, 401)
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    connection.execute("INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,"
                        "tool_call_ids,event_json,input_tokens,output_tokens) VALUES ('llm_call','call','local-session',?,"
                        "'[\"fixture-tool\"]','{}',100,50)", (time.time_ns(),))
                savings_path = "/api/token-savings?start_ns=0&end_ns=9223372036854775807"
                session_path = "/api/token-savings/session/local-session"
                expected_saved = original["before_tokens"] - original["after_tokens"]
                for path in [savings_path, session_path, "/%61pi/token-savings"]:
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        urllib.request.urlopen(sight_url + path)
                    self.assertEqual(error.exception.code, 401)
                self.assertEqual(query(savings_path)["summary"]["total_saved_tokens"], expected_saved)
                self.assertEqual(query(session_path)["total_compounded_saved"], expected_saved)
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    self.assertEqual(connection.execute("SELECT COUNT(*) FROM tokenless_statistics").fetchone()[0], 3)
                with sqlite3.connect(producer / "stats.db") as connection:
                    connection.execute("UPDATE stats SET after_tokens=after_tokens-1 WHERE id=?", (original["id"],))
                self.assertEqual(query(savings_path)["summary"]["total_saved_tokens"], expected_saved + 1)
                self.assertEqual(query(session_path)["total_compounded_saved"], expected_saved + 1)
                with sqlite3.connect(producer / "stats.db") as connection:
                    connection.execute("DELETE FROM stats WHERE id=?", (original["id"],))
                self.assertEqual(query(savings_path)["summary"]["total_saved_tokens"], 0)
                self.assertEqual(query(session_path)["total_compounded_saved"], 0)
                self.assertEqual(len(query("/api/tokenless/records?start_ns=0&end_ns=9223372036854775807")["records"]), 3)
                with sqlite3.connect(producer / "stats.db") as connection:
                    connection.execute(f"INSERT INTO stats ({','.join(original)}) VALUES ({','.join('?' for _ in original)})", list(original.values()))
                for path in ["/api/tokenless/records", "/%61pi/tokenless/records", "/api/tokenless/%72ecords"]:
                    for headers in [{}, {"Authorization": "Bearer invalid-fixture"}]:
                        with self.assertRaises(urllib.error.HTTPError) as error:
                            urllib.request.urlopen(urllib.request.Request(sight_url + path, headers=headers))
                        self.assertEqual(error.exception.code, 401)
                bearer = {"Authorization": "Bearer " + (observer / ".dashboard_token").read_text().strip()}
                with urllib.request.urlopen(urllib.request.Request(sight_url + "/api/tokenless/records", headers=bearer)) as response:
                    self.assertEqual(response.status, 200)
                # Historical compression must still count when only the final turn is visible.
                last_turn = time.time_ns() + 1_000_000_000
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    connection.executemany("INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,"
                        "tool_call_ids,event_json,input_tokens,output_tokens) VALUES ('llm_call',?,'local-session',?,?,'{}',100,50)",
                        [(f"history-call-{i}", last_turn - (499-i)*1000, json.dumps([f"history-tool-{i}"])) for i in range(500)])
                narrow = query(f"/api/token-savings?start_ns={last_turn}&end_ns={last_turn}")
                self.assertEqual(narrow["sessions"][0]["request_count"], 1)
                self.assertEqual(narrow["summary"]["total_saved_tokens"], expected_saved)
                self.assertEqual(narrow["summary"]["total_compounded_saved"], expected_saved * 500)
                self.assertEqual(query(session_path)["total_compounded_saved"], expected_saved * 500)
                # More than 1000 IDs with no source records is a complete empty result.
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    connection.executemany("INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,"
                        "tool_call_ids,event_json,input_tokens,output_tokens) VALUES ('llm_call',?,'empty-session',?,?,'{}',100,50)",
                        [(f"empty-call-{i}", last_turn+i, json.dumps([f"empty-tool-{i}"])) for i in range(501)])
                self.assertEqual(query("/api/token-savings/session/empty-session")["total_compounded_saved"], 0)
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    connection.execute("DELETE FROM genai_events WHERE call_id LIKE 'history-call-%' OR session_id='empty-session'")
                self.assertEqual(query("/api/tokenless/refresh", {})["saved"], 4)
                all_records = query("/api/tokenless/records?start_ns=0&end_ns=9223372036854775807")["records"]
                unassigned = next(row for row in all_records if row["record"]["tool_use_id"] is None)
                self.assertIsNone(unassigned["record"]["session_id"])
                self.assertIsNone(unassigned["agentsight_session_id"])
                for _ in range(2):
                    self.assertEqual(query("/api/tokenless/refresh", {"session_id": "source-session"})["saved"], 3)
                listing = query("/api/tokenless/records?start_ns=0&end_ns=9223372036854775807&session_id=source-session")
                self.assertEqual(len(listing["records"]), 3)
                self.assertNotIn(service_token, json.dumps(listing))
                self.assertEqual({row["record"]["mode"] for row in listing["records"]}, {"active", "dry-run", None})
                self.assertTrue(all(row["agentsight_session_id"] == "local-session" for row in listing["records"]))
                active = next(row for row in listing["records"] if row["record"]["mode"] == "active")
                self.assertIsNone(active["record"]["before_text"])
                page = query("/api/tokenless/records?start_ns=0&end_ns=9223372036854775807&limit=1")
                second = query(f"/api/tokenless/records?start_ns=0&end_ns=9223372036854775807&limit=1&before_cache_id={page['next_before_cache_id']}")
                self.assertNotEqual(page["records"][0]["cache_id"], second["records"][0]["cache_id"])
                savings_path = "/api/token-savings?start_ns=0&end_ns=9223372036854775807"
                expected_saved = original["before_tokens"] - original["after_tokens"]
                self.assertEqual(query(savings_path)["summary"]["total_saved_tokens"], expected_saved)
                detail_path = f"/api/tokenless/records/{active['cache_id']}?include_content=true"
                detail = query(detail_path)["record"]["record"]
                self.assertEqual(detail["before_text"], original["before_text"])
                self.assertEqual(detail["after_text"], original["after_text"])
                query("/api/tokenless/refresh", {"tool_use_id": "fixture-tool"})
                self.assertEqual(query(detail_path)["record"]["record"]["before_text"], original["before_text"])
                untouched = next(row for row in listing["records"] if row["record"]["mode"] is None)
                with sqlite3.connect(producer / "stats.db") as connection:
                    connection.execute("UPDATE stats SET timestamp='2027-01-01T00:00:00Z' WHERE id=?", (untouched["record"]["id"],))
                with self.assertRaises(urllib.error.HTTPError) as error:
                    query(f"/api/tokenless/records/{untouched['cache_id']}?include_content=true")
                self.assertEqual(error.exception.code, 409)
                with sqlite3.connect(producer / "stats.db") as connection:
                    bulk = {**original, "session_id": "bulk-scope", "tool_use_id": "bulk-tool"}
                    values = [bulk[key] for key in columns]
                    connection.executemany(f"INSERT INTO stats ({','.join(columns)}) VALUES ({','.join('?' for _ in columns)})",
                                           [values] * 1001)
                with self.assertRaises(urllib.error.HTTPError) as error:
                    query("/api/tokenless/refresh", {"session_id": "bulk-scope"})
                self.assertEqual(error.exception.code, 413)
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    connection.execute("INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,"
                        "tool_call_ids,event_json,input_tokens,output_tokens) VALUES ('llm_call','bulk-call','bulk-session',?,'[\"bulk-tool\"]','{}',100,50)", (time.time_ns(),))
                with self.assertRaises(urllib.error.HTTPError) as error:
                    query("/api/token-savings/session/bulk-session")
                self.assertEqual(error.exception.code, 413)
                with sqlite3.connect(observer / "genai_events.db") as connection:
                    self.assertEqual(connection.execute("SELECT COUNT(*) FROM tokenless_statistics").fetchone()[0], 4)
                first_source_page = query("/api/tokenless/refresh", {})
                self.assertEqual(first_source_page["saved"], 100)
                self.assertIsInstance(first_source_page["next_before_id"], str)
                second_source_page = query("/api/tokenless/refresh", {"before_id": first_source_page["next_before_id"]})
                self.assertEqual(second_source_page["saved"], 100)
                source.terminate()
                source.wait(timeout=10)
                with self.assertRaises(urllib.error.HTTPError) as error:
                    query("/api/tokenless/refresh", {"session_id": "source-session"})
                self.assertEqual(error.exception.code, 502)
                observer_process.terminate()
                observer_process.wait(timeout=10)
                sight()
                self.assertEqual(len(query("/api/tokenless/records?start_ns=0&end_ns=9223372036854775807&session_id=source-session")["records"]), 3)
                self.assertEqual(query(detail_path)["record"]["record"]["before_text"], original["before_text"])
                for path in [savings_path, session_path]:
                    with self.assertRaises(urllib.error.HTTPError) as error:
                        query(path)
                    self.assertEqual(error.exception.code, 502)
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.terminate()
                    process.wait(timeout=10)
                for log in logs:
                    log.close()


if __name__ == "__main__":
    unittest.main()
