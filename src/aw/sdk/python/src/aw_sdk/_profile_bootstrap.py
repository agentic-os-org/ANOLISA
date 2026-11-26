"""Controlled, explicit bootstrap using trusted deployment and profile files."""

from __future__ import annotations

from aw_sdk.deployment import load_descriptor, read_file
from aw_sdk.profile_source import ProfileSource, SessionIdentity
from aw_sdk.sources import SOURCE_PROTOCOL


async def bootstrap_profile(
    profile_file: str,
    config_file: str,
    descriptor_file: str,
    *,
    target: str | None = None,
    cooperative: bool = False,
    session_id_context: SessionIdentity | None = None,
    timeout_ms: int = 5000,
) -> ProfileSource:
    """Install a profile from deployment-owned files and start its SDK binding.

    The descriptor binds the local configuration and selects the endpoint and
    target. AW Service supplies the active policy. Wrap the business request
    in `async with profile.request()` or supply its SDK SourceSession to
    `profile.request(session)`. Await teardown before stopping the loop.
    If required startup fails after installation, stop the application. Its
    installed entry points stay guarded until explicit teardown or process exit.
    """
    descriptor = load_descriptor(
        config_file,
        descriptor_file,
        {"adapter": "sdk", "version": SOURCE_PROTOCOL, "entrypoint": "application"},
        target,
    )
    source = ProfileSource.probe(
        read_file(profile_file),
        availability=descriptor["availability"],
        session_id_context=session_id_context,
    )
    source.install()
    source.verify()
    await source.activate(
        endpoint=descriptor["endpoint"],
        target=descriptor["target"],
        cooperative=cooperative,
        timeout_ms=timeout_ms,
    )
    return source
