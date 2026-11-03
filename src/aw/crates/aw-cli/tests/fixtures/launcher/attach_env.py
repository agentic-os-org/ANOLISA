"""Load the native profile before serving the App fixture; procfs retains startup env."""
import json
import os
from pathlib import Path
import runpy

root = Path(os.environ["FAKE_ROOT"])
profile = runpy.run_path(str(root / "lib/qwenpaw/constant.py"))["WORKING_DIR"]
(root / "effective-profile.json").write_text(json.dumps(str(profile)))
if os.environ.get("FAKE_REMOVE_DOTENV"):
    (root / ".env").unlink()
runpy.run_path(os.environ["FAKE_APP_SOURCE"], run_name="__main__")
