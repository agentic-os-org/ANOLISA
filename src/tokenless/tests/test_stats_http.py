#!/usr/bin/env python3
"""Exercise the real CLI writer and standalone authenticated query process."""

from __future__ import annotations

import json
import http.client
from contextlib import closing
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import time
from typing import Any
import unittest
import urllib.error
import urllib.request

BINARY = Path(__file__).resolve().parents[1] / "target/debug/tokenless"


class StatisticsHttpTest(unittest.TestCase):
    def test_cli_writer_query_after_exit_and_read_only_failures(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            environment = {**os.environ, "TOKENLESS_DATA_DIR": temporary,
                           "TOKENLESS_STATS_ENABLED": "1", "TOKENLESS_SLS_ENABLED": "0",
                           "TOKENLESS_COMPRESSION_ENABLED": "1"}
            original = json.dumps({"output": "repeated fixture output " * 300})
            subprocess.run([str(BINARY), "compress-response", "--agent-id", "fixture",
                            "--session-id", "fixture-session", "--tool-use-id", "fixture-tool",
                            "--no-stash", "--truncate-strings-at", "64"], input=original,
                           text=True, env=environment, capture_output=True, check=True)
            connection = sqlite3.connect(directory / "stats.db")
            stored = connection.execute("SELECT id,timestamp,before_tokens,after_tokens,before_text,after_text FROM stats").fetchone()
            self.assertIsNotNone(stored)
            connection.close()
            token = secrets.token_urlsafe(32)
            credential = directory / "credential"
            credential.write_text(token + "\n")
            credential.chmod(0o600)
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            process = subprocess.Popen([str(BINARY), "stats", "serve", "--data-dir", temporary,
                                        "--credential-file", str(credential), "--listen", f"127.0.0.1:{port}"],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                base = f"http://127.0.0.1:{port}/v1/stats/records"
                for _ in range(100):
                    if process.poll() is not None:
                        self.fail(process.stderr.read().decode())
                    try:
                        urllib.request.urlopen(base, timeout=0.2)
                    except urllib.error.HTTPError as error:
                        self.assertEqual(error.code, 401)
                        break
                    except urllib.error.URLError:
                        time.sleep(0.05)
                else:
                    self.fail("query service did not start")

                # Send headers without a body: auth/method checks must not wait for it.
                for authorization, method, path, expected in [
                    ("", "POST", "/v1/stats/records/query", 401),
                    ("Authorization: Bearer wrong\r\n", "POST", "/v1/stats/records/query", 401),
                    (f"Authorization: Bearer {token}\r\n", "GET", "/v1/stats/records/query", 405),
                    ("", "POST", "/totally/unknown", 401),
                    (f"Authorization: Bearer {token}\r\n", "POST", "/totally/unknown", 404),
                ]:
                    with socket.create_connection(("127.0.0.1", port), timeout=2) as client:
                        client.sendall((f"{method} {path} HTTP/1.1\r\n"
                                        f"Host: localhost\r\n{authorization}"
                                        "Content-Length: 1048576\r\nConnection: close\r\n\r\n").encode())
                        response = b""
                        while b"\r\n\r\n" not in response:
                            chunk = client.recv(4096)
                            self.assertTrue(chunk)
                            response += chunk
                        self.assertIn(f" {expected} ".encode(), response.split(b"\r\n")[0])
                        if expected == 405:
                            self.assertIn(b"allow: post", response.lower())

                def query(suffix: str) -> dict[str, Any]:
                    request = urllib.request.Request(base + suffix, headers={"Authorization": f"Bearer {token}"})
                    with urllib.request.urlopen(request, timeout=5) as response:
                        self.assertEqual(response.headers["Cache-Control"], "no-store")
                        return json.load(response)

                page = query("?session_id=fixture-session&tool_use_id=fixture-tool")
                self.assertEqual(len(page["records"]), 1)
                record = page["records"][0]
                self.assertEqual(record["id"], stored[0])
                self.assertEqual(record["timestamp"], stored[1])
                self.assertEqual(record["before_tokens"], stored[2])
                self.assertEqual(record["after_tokens"], stored[3])
                self.assertEqual(record["mode"], "active")
                self.assertFalse(record["content_included"])
                detail = query(f"/{stored[0]}?include_content=true")["record"]
                self.assertEqual(detail["before_text"], stored[4])
                self.assertEqual(detail["after_text"], stored[5])
                self.assertTrue(detail["content_included"])

                # The real server must suppress HEAD bodies while retaining GET metadata.
                for suffix, expected in [("", 200), (f"/{stored[0]}?include_content=true", 200),
                                         ("/999999", 404), ("?limit=0", 400)]:
                    responses = []
                    for method in ["GET", "HEAD"]:
                        with socket.create_connection(("127.0.0.1", port), timeout=2) as client:
                            client.sendall((f"{method} /v1/stats/records{suffix} HTTP/1.1\r\n"
                                            f"Host: localhost\r\nAuthorization: Bearer {token}\r\n"
                                            "Connection: close\r\n\r\n").encode())
                            wire = b""
                            while True:
                                chunk = client.recv(4096)
                                if not chunk:
                                    break
                                wire += chunk
                        headers, _, body = wire.partition(b"\r\n\r\n")
                        self.assertIn(f" {expected} ".encode(), headers.split(b"\r\n")[0])
                        fields = dict(line.lower().split(b": ", 1)
                                      for line in headers.split(b"\r\n")[1:])
                        responses.append((fields, body))
                    self.assertEqual(responses[1][1], b"")
                    for name in [b"content-type", b"cache-control", b"content-length"]:
                        self.assertEqual(responses[0][0][name], responses[1][0][name])
                    self.assertEqual(int(responses[0][0][b"content-length"]), len(responses[0][1]))

                for method, path, status, allow in [
                    ("POST", "/totally/unknown", 404, None),
                    ("POST", "/v1/stats/records/", 404, None),
                    ("GET", "/v1/stats/records/", 404, None),
                    ("GET", "/v1/stats/records/not-an-id", 404, None),
                    ("POST", "/v1/stats/records", 405, "GET, HEAD"),
                    ("POST", f"/v1/stats/records/{stored[0]}", 405, "GET, HEAD"),
                    ("HEAD", "/v1/stats/records/query", 405, "POST"),
                ]:
                    with closing(http.client.HTTPConnection("127.0.0.1", port, timeout=2)) as client:
                        client.request(method, path, headers={"Authorization": f"Bearer {token}"})
                        result = client.getresponse()
                        self.assertEqual(result.status, status)
                        self.assertEqual(result.getheader("Allow"), allow)
                        result.read()
                connection = sqlite3.connect(directory / "stats.db")
                connection.execute("UPDATE stats SET timestamp='private malformed fixture'")
                connection.commit()
                with self.assertRaises(urllib.error.HTTPError) as corrupt:
                    query("")
                self.assertEqual(corrupt.exception.code, 500)
                diagnostic = json.load(corrupt.exception)
                self.assertEqual(diagnostic["error"], {
                    "code": "invalid_record", "record_id": stored[0], "field": "timestamp"})
                self.assertNotIn("private malformed", json.dumps(diagnostic))
                connection.execute("UPDATE stats SET timestamp=?", (stored[1],))
                connection.commit()
                connection.close()
                self.assertEqual(len(query("")["records"]), 1)
                (directory / "stats.db").rename(directory / "original.db")
                with self.assertRaises(urllib.error.HTTPError) as failure:
                    query("")
                self.assertEqual(failure.exception.code, 503)
                self.assertFalse((directory / "stats.db").exists())
            finally:
                process.terminate()
                process.wait(timeout=10)
                process.stderr.close()

    @unittest.skipUnless(os.name == "posix", "Unix credential permissions")
    def test_remote_plaintext_and_shared_credential_warnings(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            connection = sqlite3.connect(directory / "stats.db")
            connection.execute("CREATE TABLE stats(id INTEGER PRIMARY KEY,timestamp TEXT,"
                               "operation TEXT,agent_id TEXT,before_chars INTEGER,"
                               "before_tokens INTEGER,after_chars INTEGER,after_tokens INTEGER)")
            connection.close()
            credential = directory / "credential"
            credential.write_text(secrets.token_urlsafe(32))
            credential.chmod(0o644)
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                port = listener.getsockname()[1]
            with (directory / "stderr").open("w+") as stderr:
                process = subprocess.Popen(
                    [str(BINARY), "stats", "serve", "--data-dir", temporary,
                     "--credential-file", str(credential), "--listen", f"0.0.0.0:{port}"],
                    stdout=subprocess.DEVNULL, stderr=stderr)
                try:
                    for _ in range(100):
                        if process.poll() is not None:
                            self.fail("query service exited before listening")
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                                break
                        except OSError:
                            time.sleep(0.05)
                    else:
                        self.fail("query service did not start")
                finally:
                    process.terminate()
                    process.wait(timeout=10)
                stderr.seek(0)
                warnings = stderr.read()
            self.assertIn("plaintext HTTP", warnings)
            self.assertIn("TLS proxy", warnings)
            self.assertIn("group/other access", warnings)
            self.assertIn("0600", warnings)
            self.assertNotIn(credential.read_text(), warnings)

    def test_source_index_warning_does_not_migrate_database(self) -> None:
        for indexed in [False, True]:
            with self.subTest(indexed=indexed), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                database = directory / "stats.db"
                with sqlite3.connect(database) as connection:
                    connection.execute("CREATE TABLE stats(id INTEGER PRIMARY KEY,timestamp TEXT,"
                                       "operation TEXT,agent_id TEXT,before_chars INTEGER,"
                                       "before_tokens INTEGER,after_chars INTEGER,after_tokens INTEGER,"
                                       "tool_use_id TEXT)")
                    if indexed:
                        connection.execute("CREATE INDEX idx_tool_use_id ON stats(tool_use_id)")
                original = database.read_bytes()
                credential = directory / "credential"
                credential.write_text(secrets.token_urlsafe(32))
                credential.chmod(0o600)
                with socket.socket() as listener:
                    listener.bind(("127.0.0.1", 0))
                    port = listener.getsockname()[1]
                with (directory / "stderr").open("w+") as stderr:
                    process = subprocess.Popen(
                        [str(BINARY), "stats", "serve", "--data-dir", temporary,
                         "--credential-file", str(credential), "--listen", f"127.0.0.1:{port}"],
                        stdout=subprocess.DEVNULL, stderr=stderr)
                    try:
                        for _ in range(100):
                            if process.poll() is not None:
                                self.fail("query service exited before listening")
                            try:
                                request = urllib.request.Request(
                                    f"http://127.0.0.1:{port}/v1/stats/records",
                                    headers={"Authorization": f"Bearer {credential.read_text()}"})
                                with urllib.request.urlopen(request, timeout=0.2) as response:
                                    self.assertEqual(json.load(response)["records"], [])
                                    break
                            except urllib.error.URLError:
                                time.sleep(0.05)
                        else:
                            self.fail("query service did not start")
                    finally:
                        process.terminate()
                        process.wait(timeout=10)
                    stderr.seek(0)
                    warnings = stderr.read()
                self.assertEqual(warnings.count("source lacks idx_tool_use_id"), 0 if indexed else 1)
                if not indexed:
                    self.assertIn("upgraded native recorder", warnings)
                self.assertNotIn(credential.read_text(), warnings)
                self.assertEqual(database.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
