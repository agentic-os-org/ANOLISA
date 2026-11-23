"""Isolated real Skill Ledger daemon with test-only directory authorization."""

import json
import os
from pathlib import Path
import subprocess
import uuid

from runtime import await_condition, checked, save


class SkillBackend:
    def __init__(self, root, output, binaries, image):
        self.root, self.output, self.binaries, self.image = root, output, binaries, image
        self.runtime = root / "sec-runtime"
        self.runtime.mkdir(mode=0o755)
        self.runtime.chmod(0o755)
        self.socket = self.runtime / "daemon.sock"
        self.name = "aw-skill-" + uuid.uuid4().hex[:16]
        self.created = False
        self.paused = False

    def start(self):
        config = {"stateDir":"/var/lib/agent-sec/skillsec", "enableDefaultSkillDirs":False,
            "managedSkillDirs":[str(self.root / path) for path in
                ("work/.qoder/skills/*", "work/.agents/skills/*", "config/skills/*", "work/skills/*")]}
        script = """set -eu
umask 077
chown 0:0 /run/agent-sec-core
mkdir -p /etc/agent-sec /var/log/agent-sec
printf '%s' "$1" > /etc/agent-sec/skillsec.json
exec /app/agent-sec-daemon --socket /run/agent-sec-core/daemon.sock --skillsec-config /etc/agent-sec/skillsec.json
"""
        command = ["docker", "create", "--name", self.name,
            "--label", "aw.native-owner=" + (os.environ.get("AW_NATIVE_OWNER") or self.name),
            "--network", "none", "--security-opt", "no-new-privileges",
            "--mount", f"type=bind,source={self.root},target={self.root}",
            "--mount", f"type=bind,source={self.runtime},target=/run/agent-sec-core",
            "--mount", f'type=bind,source={self.binaries / "agent-sec-daemon"},target=/app/agent-sec-daemon,readonly',
            self.image, "/bin/sh", "-c", script, "sh", json.dumps(config)]
        result = checked(command)
        self.created = True
        checked(["docker", "start", self.name])
        save(self.output / "backend.json", {"container":self.name,"id":result.stdout.strip(),
            "command":command, "socket":str(self.socket), "ports":[],
            "stop":f"docker rm -f {self.name}", "deadline_seconds":900})
        await_condition(lambda: self.socket.exists(), 15, "Skill daemon ready")

    def cli(self, *args):
        result = subprocess.run([str(self.binaries / "agent-sec-cli"), "--socket", str(self.socket),
            *map(str,args)], capture_output=True, text=True, timeout=20,
            env={k:v for k,v in os.environ.items() if k != "QODER_PERSONAL_ACCESS_TOKEN"})
        return {"exit_code":result.returncode,"stdout":result.stdout,"stderr":result.stderr}

    def pause(self):
        checked(["docker", "pause", self.name])
        self.paused = True

    def resume(self):
        if self.paused:
            checked(["docker", "unpause", self.name])
            self.paused = False

    def tamper(self, directory):
        checked(["docker", "exec", self.name, "/bin/sh", "-c",
            'printf "%s" "{}" > "$1"', "sh", str(directory / ".skill-meta/latest.json")])

    def close(self):
        if not self.created:
            return
        try:
            self.resume()
            logs = checked(["docker", "logs", self.name])
            (self.output / "backend.log").write_text(logs.stdout + logs.stderr)
        finally:
            checked(["docker", "rm", "-f", self.name])
            # Signing metadata and the socket are root-owned. Change ownership
            # only inside this fixture's exact mount before temporary cleanup.
            checked(["docker", "run", "--rm", "--network", "none", "--mount",
                f"type=bind,source={self.root},target=/data", self.image,
                "chown", "-R", f"{os.getuid()}:{os.getgid()}", "/data"])
            self.created = False
            save(self.output / "backend-cleanup.json", {"container":self.name,"removed":True})
