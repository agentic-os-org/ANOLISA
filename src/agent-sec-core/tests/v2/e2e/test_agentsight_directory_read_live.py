"""Run with ASC_TEST_AGENTSIGHT_LIVE=1 in an isolated root Linux guest.

Start agentsight serve on its default address with dashboard authentication
and a real enforcer using agent-file-guard, BPF LSM, and private BPF pins.
Put the V2 binaries on PATH and use a non-overlay filesystem for --basetemp.
"""

import errno
import json
import os
import subprocess
import sys
import time
import uuid
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING, Any
from urllib.request import Request, urlopen

import pytest

if TYPE_CHECKING:
    from tests.v2.e2e.conftest import DaemonHandle

pytestmark = pytest.mark.skipif(
    os.environ.get("ASC_TEST_AGENTSIGHT_LIVE") != "1",
    reason="requires isolated Linux with a real agent-file-guard enforcer",
)

_TOKEN = Path("/var/log/sysak/.agentsight/.dashboard_token")
_WORKER = """
import json, mmap, os, sys
from pathlib import Path

fd = os.open(sys.argv[1], os.O_RDONLY)
mapping = None
print("ready", flush=True)
for line in sys.stdin:
    action, *paths = json.loads(line)
    try:
        if action == "read":
            Path(paths[0]).read_bytes()
        elif action == "child_read":
            pid = os.fork()
            if not pid:
                try:
                    Path(paths[0]).read_bytes()
                except OSError as error:
                    os._exit(error.errno)
                os._exit(0)
            print(os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1]), flush=True)
            continue
        elif action == "readfd":
            os.pread(fd, 1, 0)
        elif action == "mmap":
            with mmap.mmap(fd, 0, access=mmap.ACCESS_READ) as region:
                region[0]
        elif action == "holdmap":
            with open(paths[0], "rb") as file:
                mapping = mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ)
        elif action == "unmap":
            mapping.close()
        elif action == "write":
            with open(paths[0], "ab") as file:
                file.write(b"x")
        elif action == "unlink":
            os.unlink(paths[0])
        elif action == "stat":
            os.stat(paths[0])
        elif action == "list":
            os.listdir(paths[0])
        elif action == "link":
            os.link(*paths)
        elif action == "rename":
            os.rename(*paths)
        else:
            raise ValueError(action)
    except OSError as error:
        print(error.errno, flush=True)
    else:
        print(0, flush=True)
"""


def _sight(path: str) -> dict[str, Any]:
    request = Request(
        f"http://127.0.0.1:7396/api/enforcement/{path}",
        headers={"Authorization": f"Bearer {_TOKEN.read_text().strip()}"},
    )
    with urlopen(request, timeout=5) as response:
        return json.load(response)


def _wait_for(probe: Callable[[], Any], ready: Callable[[Any], bool]) -> Any:
    deadline = time.monotonic() + 30
    while True:
        value = probe()
        if ready(value):
            return value
        if time.monotonic() >= deadline:
            pytest.fail(f"Live enforcement did not converge: {value!r}")
        time.sleep(0.1)


@contextmanager
def _assignment(
    daemon: "DaemonHandle", source: Path, pid: int, cedar: str
) -> Iterator[dict[str, Any]]:
    source.write_text(
        json.dumps(
            {
                "language": "cedar",
                "content": {
                    "schema": {"schemaId": "seccore.system", "revision": 1},
                    "text": cedar,
                },
            }
        )
    )
    policy = daemon.request(
        "policy",
        "create",
        "--name",
        str(uuid.uuid4()),
        "--language",
        "json",
        "--file",
        str(source),
    )
    scope = daemon.request(
        "scope",
        "create",
        "--pid",
        str(pid),
        "--policy-id",
        policy["policyId"],
        "--policy-revision",
        "1",
    )
    try:
        listing = _wait_for(
            lambda: daemon.request("binding", "list"),
            lambda value: value["total"] == 1
            and value["items"][0]["status"]["phase"] in {"READY", "APPLY_FAILED"},
        )
        yield listing["items"][0]
    finally:
        daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
        _wait_for(
            lambda: daemon.request("binding", "list"), lambda value: value["total"] == 0
        )


@pytest.fixture
def bind_alias(tmp_path: Path) -> Iterator[Path]:
    tree = tmp_path / "secrets"
    alias = tmp_path / "bind-alias"
    tree.mkdir()
    alias.mkdir()
    subprocess.run(["mount", "--bind", str(tree), str(alias)], check=True)
    try:
        yield alias
    finally:
        subprocess.run(["umount", str(alias)], check=True)


def test_directory_read_policy_on_a_live_kernel(
    start_daemon: Callable[[], "DaemonHandle"],
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    bind_alias: Path,
) -> None:
    health = _sight("health")
    assert health["ready"] and health["backend"] == "actplane", health
    assert health["capabilities"]["directory_tree_read_protection"], health
    assert not _sight("bindings")["bindings"], "use a dedicated enforcer for this test"
    # The fixture captures daemon output in a pipe until shutdown.
    monkeypatch.setenv("RUST_LOG", "warn")
    daemon = start_daemon()

    tree = tmp_path / "secrets"
    deep = tree / "a/b/c/deep"
    public = tree / "public"
    for directory in (deep.parent, public):
        directory.mkdir(parents=True)
    existing, replaced = tree / "existing", tree / "replaced"
    excluded, outside = public / "readme", tmp_path / "outside"
    for path in (existing, replaced, deep, excluded, outside):
        path.write_bytes(b"fixture")
    symlink = tmp_path / "symlink"
    symlink.symlink_to(existing)
    tree_match = f'(resource.path == "{tree}" || resource.path like "{tree}/*")'
    read = 'forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)'
    cedar = (
        f"{read} when {{ {tree_match} }} "
        f'unless {{ resource.path == "{public}" || resource.path like "{public}/*" }};'
    )
    source = tmp_path / "policy.json"
    denied = errno.EPERM
    with subprocess.Popen(
        [sys.executable, "-u", "-c", _WORKER, str(existing)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    ) as process:
        assert process.stdout.readline().strip() == "ready"

        def check(expected: int, operation: str, *paths: Path) -> None:
            process.stdin.write(json.dumps([operation, *map(str, paths)]) + "\n")
            process.stdin.flush()
            actual = process.stdout.readline().strip()
            assert actual == str(expected), (operation, paths, actual)

        try:
            check(0, "readfd")
            check(0, "mmap")
            check(0, "holdmap", bind_alias / existing.name)
            with _assignment(daemon, source, process.pid, cedar) as rejected:
                assert rejected["status"]["phase"] == "APPLY_FAILED", rejected
                assert (
                    rejected["status"]["error"]["code"] == "AGENTSIGHT_COMPILE_FAILURE"
                )
                check(0, "readfd")
            check(0, "unmap")
            hardlink = tmp_path / "hardlink"
            os.link(existing, hardlink)
            with _assignment(daemon, source, process.pid, cedar) as rejected:
                assert rejected["status"]["phase"] == "APPLY_FAILED", rejected
                assert (
                    rejected["status"]["error"]["code"] == "AGENTSIGHT_COMPILE_FAILURE"
                )
            hardlink.unlink()

            with _assignment(daemon, source, process.pid, cedar) as binding:
                assert binding["status"] == {"phase": "READY"}, binding
                spec = binding["spec"]
                binding_id = str(
                    uuid.uuid5(
                        uuid.NAMESPACE_URL,
                        f"urn:agentseccore:agentsight-binding:{spec['bindingId']}:revision:1",
                    )
                )
                (receipt,) = _sight("bindings")["bindings"]
                assert receipt["state"] == "enforced"
                assert receipt["request"]["binding_id"] == binding_id
                assert receipt["request"]["file_protection_policy"] == {
                    "version": 1,
                    "topology_mode": "static_at_apply",
                    "targets": [
                        {
                            "kind": "directory_tree",
                            "path": str(tree),
                            "protections": {
                                "delete": False,
                                "ordinary_write": False,
                                "read": True,
                            },
                            "exclusions": [
                                {"kind": "directory_subtree", "path": str(public)}
                            ],
                        }
                    ],
                }
                created, moved = tree / "created", tree / "moved"
                created.write_bytes(b"created")
                incoming = tmp_path / "incoming"
                incoming.write_bytes(b"moved")
                incoming.rename(moved)
                replacement = tmp_path / "replacement"
                replacement.write_bytes(b"replacement")
                replacement.replace(replaced)
                for path in (
                    existing,
                    deep,
                    created,
                    moved,
                    replaced,
                    symlink,
                    bind_alias / existing.name,
                ):
                    check(denied, "read", path)
                check(denied, "child_read", deep)
                check(denied, "readfd")
                check(denied, "mmap")
                check(denied, "link", existing, hardlink)
                check(denied, "rename", existing, tmp_path / "moved-out")
                for path in (excluded, outside):
                    check(0, "read", path)
                check(0, "list", tree)
                check(0, "stat", existing)
                check(0, "write", existing)
                check(0, "unlink", created)
                assert replaced.read_bytes() == b"replacement"
                expected = {
                    "binding_id": binding_id,
                    "policy_id": spec["policy"]["policyId"],
                    "policy_revision": "1",
                    "pid": process.pid,
                    "process_start_time": spec["scope"]["process"]["startTime"],
                    "operation": "read",
                    "blocked": True,
                    "rule_id": "file-protection-target-1",
                }
                events = _wait_for(
                    lambda: _sight("violations")["violations"],
                    lambda events: any(
                        all(event[key] == value for key, value in expected.items())
                        and str(tree) in (event["reason"] or "")
                        for event in events
                    ),
                )
                print(
                    f"Kernel read denial matched Binding, Policy and process {expected}"
                )
                print(f"AgentSight returned {len(events)} violation events")
            assert not _sight("bindings")["bindings"]
            check(0, "read", existing)
            check(0, "readfd")
            check(0, "mmap")

            with _assignment(
                daemon,
                source,
                process.pid,
                f'{read} when {{ resource.path == "{existing}" }};',
            ) as binding:
                assert binding["status"] == {"phase": "READY"}, binding
                check(denied, "readfd")
                check(0, "read", deep)
            write = 'forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)'
            operations = (
                '[SecCore::WriteOperation::"delete", SecCore::WriteOperation::"write"]'
                ".contains(context.input.operation)"
            )
            with _assignment(
                daemon,
                source,
                process.pid,
                f"{write} when {{ {operations} && {tree_match} }};",
            ) as binding:
                assert binding["status"] == {"phase": "READY"}, binding
                check(denied, "write", existing)
                check(denied, "unlink", existing)
                check(0, "read", existing)
        finally:
            process.stdin.close()
            process.wait(timeout=5)
