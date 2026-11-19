"""Use the deployment configuration while retaining native QwenPaw startup."""

import os

from aw_sdk.qwenpaw import QwenPawPlugin

plugin = QwenPawPlugin.from_config(os.environ["AW_CONFIG"])
