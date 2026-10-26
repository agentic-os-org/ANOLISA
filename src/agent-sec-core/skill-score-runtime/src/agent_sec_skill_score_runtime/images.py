"""Versioned images delivered by the optional Skill Score runtime."""

SIDECAR_IMAGE = "agent-sec-skill-score-egress:1"


def sidecar_image() -> str:
    """Return the ASC-owned image name without consulting Harbor internals."""
    return SIDECAR_IMAGE
