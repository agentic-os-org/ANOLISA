"""Count real CLI/Service cleanup RPCs and inject bounded transport failures."""

import json
from pathlib import Path
import socket
import struct
import subprocess
import sys
import threading


def provider():
    request = json.load(sys.stdin)
    reply = {"api_version": request["api_version"], "request_id": request["request_id"], "status": "ok"}
    if request["method"] == "describe":
        reply["operations"] = [{"name": name, "events": ["tool.before"], "effects": ["block"]}
                               for name in ["first", "last"]]
    elif request["method"] == "invoke":
        native = request["event"]["native"]
        fail = request["operation"] == native["tool_input"]["stop_step"]
        mode = native["tool_input"]["mode"]
        if fail and mode in ["error", "report"]:
            sys.exit(9)
        reply.update(input_digest=request["input_digest"], effects=[
            {"type": "block", "reason_code": "fixture_denied"}
        ] if fail and mode in ["deny", "cleanup-error"] else [])
    print(json.dumps(reply))


def frame(stream):
    def exact(size):
        data = b""
        while len(data) < size:
            chunk = stream.recv(size - len(data))
            if not chunk:
                raise EOFError("incomplete frame")
            data += chunk
        return data
    size = exact(4)
    return size + exact(struct.unpack(">I", size)[0])


def run():
    root, native, executable = map(Path, sys.argv[1:])
    binding = json.loads((root / "binding.json").read_text())
    original = binding["socket"]
    binding["socket"] = str(root / "proxy.sock")
    (root / "binding.json").write_text(json.dumps(binding))
    stop = threading.Event()
    records, errors, workers = [], [], []

    def handle(connection):
        try:
            with connection:
                connection.settimeout(8)
                raw = frame(connection)
                request = json.loads(raw[4:])
                operation = request["operation"]
                method = operation["method"]
                records.append(method)
                mode = (root / "mode").read_text()
                if mode == "cleanup-error" and method in ["close_event", "close_hook_event"]:
                    return
                if mode == "abort" and method == "invoke_step" and operation["step_id"] == (root / "step").read_text():
                    (root / "abort-ready").write_text("ready")
                    # The bridge kills its callback transport, then closes by native identity.
                    assert connection.recv(1) == b""
                    return
                with socket.socket(socket.AF_UNIX) as upstream:
                    upstream.settimeout(8)
                    upstream.connect(original)
                    upstream.sendall(raw)
                    connection.sendall(frame(upstream))
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:
            errors.append(str(error))

    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(binding["socket"])
        listener.listen()
        listener.settimeout(0.1)

        def accept():
            while not stop.is_set():
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                worker = threading.Thread(target=handle, args=(connection,))
                workers.append(worker)
                worker.start()

        thread = threading.Thread(target=accept)
        thread.start()
        try:
            result = subprocess.run(["node", str(Path(__file__).with_suffix(".mjs")),
                                     str(root), str(native), str(executable)], timeout=9)
        finally:
            stop.set()
            thread.join(1)
            for worker in workers:
                worker.join(9)
                assert not worker.is_alive()
        assert not errors, errors
        assert result.returncode == 0
    (root / "rpc.json").write_text(json.dumps(records))
    print(json.dumps(records))


if __name__ == "__main__":
    if sys.argv[1:] == ["provider"]:
        provider()
    else:
        run()
