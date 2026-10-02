"""Hermes plugin entry point — agent-sec-core security guardrails."""

from __future__ import annotations

import logging
from pathlib import Path

from .capabilities import ALL_CAPABILITIES
from .registry import load_config, register_capabilities

logger = logging.getLogger("agent-sec-core")


def register(ctx):
    """Hermes plugin entry point.

    Called once at startup by the Hermes plugin framework.
    Loads configuration and registers all enabled security capabilities.
    """
    plugin_dir = Path(__file__).parent
    config = load_config(plugin_dir)
    register_capabilities(ctx, ALL_CAPABILITIES, config)
    logger.info("[agent-sec-core] plugin loaded")
