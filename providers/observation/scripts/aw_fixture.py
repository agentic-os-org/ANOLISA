"""Own an isolated real AW daemon, with no production state or configured Hooks."""

import json
import os
from pathlib import Path
import subprocess

from run_case import stop, wait_for


class AwFixture:
    def __init__(self, aw, directory):
        self.aw = str(aw)
        self.directory = directory
        directory.mkdir(mode=0o700)
        self.state = directory / "state"
        self.state.mkdir(mode=0o700)
        config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "ptrace-fixture"},
                  "spec": {"daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "auto"},
                           "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                           "audit": {"enabled": True, "payload": "metadata_only"},
                           "agents": {"fixture": {"adapter": "qoder-sdk", "argv": ["/bin/true"]}},
                           "providers": {}, "events": {}}}
        configuration = directory / "aw.json"
        configuration.write_text(json.dumps(config))
        with (directory / "daemon.log").open("w") as log:
            self.process = subprocess.Popen([self.aw, "serve", "--config", str(configuration),
                                             "--state-dir", str(self.state)], stdout=log, stderr=log)
        try:
            wait_for(lambda: list(self.state.rglob("*.sock")) or self.process.poll() is not None)
            sockets = list(self.state.rglob("*.sock"))
            if self.process.poll() is not None or len(sockets) != 1:
                raise RuntimeError((directory / "daemon.log").read_text())
            self.socket = str(sockets[0])
            self.status()
        except BaseException:
            stop(self.process)
            raise

    def status(self):
        completed = subprocess.run([self.aw, "request", "--socket", self.socket],
                                   input=json.dumps({"method": "status"}), capture_output=True, text=True, timeout=10)
        if completed.returncode:
            raise RuntimeError(completed.stderr)
        return json.loads(completed.stdout)["result"]

    def close(self):
        stop(self.process)
