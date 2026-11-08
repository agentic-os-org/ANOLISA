# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Set the mock service memory limit before executing its command."""

from __future__ import annotations

import os
import resource
import signal
import sys


def main() -> None:
    """Apply the existing limit and replace this process with the service."""
    limit = 512 * 1024 * 1024
    resource.setrlimit(resource.RLIMIT_AS, (limit, limit))

    # Match subprocess.Popen's default signal restoration. Python resets
    # these signals while starting this small launcher.
    for name in ("SIGPIPE", "SIGXFZ", "SIGXFSZ"):
        signum = getattr(signal, name, None)
        if signum is not None:
            signal.signal(signum, signal.SIG_DFL)

    command = sys.argv[1:]
    os.execvp(command[0], command)


if __name__ == "__main__":
    main()
