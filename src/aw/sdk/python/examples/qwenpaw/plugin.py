"""Install as a native QwenPaw plugin; retain the application's ordinary startup."""

import os

from aw_sdk import Instance
from aw_sdk.qwenpaw import QwenPawPlugin

instance = Instance(
    endpoint=os.environ["AW_SOCKET"],
    target="qwenpaw",
    availability="native_fallback",
    capabilities={
        "adapter": "qwenpaw",
        "version": "public-middleware/v1",
        "entrypoint": "app",
        "events": {
            "input.submit": ["observe", "block"],
            "tool.before": ["observe", "block", "replace_input"],
            "tool.after": ["observe", "replace_result"],
        },
    },
    install=lambda routes: plugin.ready(routes),
)
plugin = QwenPawPlugin(instance)
