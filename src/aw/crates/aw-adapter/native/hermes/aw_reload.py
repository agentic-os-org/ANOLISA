"""Hold native chat admission through model, tool and callback cleanup."""

import json
import os
import subprocess
import threading
from typing import Any, Callable
from uuid import uuid4


class Runtime:
    def __init__(self, launch: dict[str, Any], active_tools: Callable[[], int]) -> None:
        self.launch = launch
        self.active_tools = active_tools
        self.lock = threading.Lock()
        self.busy = False
        self.failed = False
        self.pending_input = False
        self.owner = None
        self.key = None

    def lifecycle(self, phase: str, key: str = "") -> None:
        result = subprocess.run([
            self.launch["binary"], "bridge", "--adapter", "hermes",
            "--binding", self.launch["binding"],
        ], input=json.dumps({"phase": phase, "key": key, "pid": os.getpid()}).encode(),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5, check=False)
        if result.returncode != 0:
            self.failed = True
            raise RuntimeError("AW native chat lifecycle failed")

    def ready(self) -> None:
        self.lifecycle("ready")

    def admit_input(self) -> None:
        with self.lock:
            if self.busy or self.failed:
                raise RuntimeError("AW native chat cleanup is unconfirmed; restart required")
            self.busy = True
            self.pending_input = True
            self.owner = threading.get_ident()
            self.key = str(uuid4())
        self.lifecycle("request_start", self.key)

    def cancel_input(self) -> None:
        if not self.pending_input or self.owner != threading.get_ident() or self.active_tools():
            raise RuntimeError("AW native input cleanup is unconfirmed")
        self.lifecycle("request_end", self.key)
        with self.lock:
            self.busy = False
            self.pending_input = False

    def chat(self, callback: Callable[..., Any], *args: Any, **kwargs: Any) -> Any:
        with self.lock:
            pending = self.pending_input and self.owner == threading.get_ident()
            if self.failed or (self.busy and not pending):
                raise RuntimeError("AW native chat cleanup is unconfirmed; restart required")
            self.pending_input = False
            self.busy = True
            if not pending:
                self.key = str(uuid4())
        try:
            if not pending:
                self.lifecycle("request_start", self.key)
            result = callback(*args, **kwargs)
            # Native cancellation returns None; it does not prove tool cleanup.
            if result is not None and self.active_tools() == 0:
                self.lifecycle("request_end", self.key)
                with self.lock:
                    self.busy = False
            else:
                self.failed = True
            return result
        except BaseException:
            self.failed = True
            raise
