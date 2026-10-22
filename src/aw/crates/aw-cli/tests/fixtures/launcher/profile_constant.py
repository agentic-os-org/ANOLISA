"""Pinned profile-selection seam; local validation also loads the real QwenPaw module."""
import os
from pathlib import Path

env_path = Path(__file__).resolve().parent.parent.parent / ".env"
if not os.environ.get("QWENPAW_RUNTIME_ID") and env_path.exists():
    # Fixtures use literal bindings only. The pinned dotenv loader also preserves
    # every initially present variable, including empty values.
    for line in env_path.read_text().splitlines():
        name, value = line.split("=", 1)
        os.environ.setdefault(name, value)

explicit = os.environ.get("QWENPAW_WORKING_DIR")
legacy = Path("~/.copaw").expanduser()
WORKING_DIR = (Path(explicit).expanduser() if explicit else
               legacy if legacy.exists() else Path("~/.qwenpaw").expanduser()).resolve()
