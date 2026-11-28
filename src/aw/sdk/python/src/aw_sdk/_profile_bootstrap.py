"""Controlled, explicit bootstrap using trusted deployment and profile files."""

from __future__ import annotations

import hashlib

from aw_sdk._profile_eval import parse_profile
from aw_sdk.deployment import SDK_PROFILE, load_descriptor, read_file
from aw_sdk.profile_source import ProfileSource, SessionIdentity
from aw_sdk.transport import AwError


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
        SDK_PROFILE,
        target,
    )
    document = read_file(profile_file)
    if "instrumentation_profiles" in descriptor:
        profile = parse_profile(document)
        configured_payload({profile["id"]: document.decode()}, descriptor)
    source = ProfileSource.probe(
        document,
        availability=descriptor["availability"],
        session_id_context=session_id_context,
    )
    verify_capabilities(source, descriptor)
    source.install()
    source.verify()
    await source.activate(
        endpoint=descriptor["endpoint"],
        target=descriptor["target"],
        cooperative=cooperative,
        timeout_ms=timeout_ms,
    )
    return source


def configured_payload(documents: object, descriptor: dict) -> tuple[str, ...]:
    binding = descriptor.get("instrumentation_profiles")
    if (
        not binding
        or binding["runtime"] != "python"
        or not isinstance(documents, dict)
        or documents.keys() != binding["profiles"].keys()
    ):
        raise AwError("source_profile_binding_mismatch")
    for name, document in documents.items():
        if (
            not isinstance(document, str)
            or hashlib.sha256(document.encode()).hexdigest()
            != binding["profiles"][name]
        ):
            raise AwError("source_profile_binding_mismatch")
        profile = parse_profile(document)
        if profile["id"] != name or profile["origin"] != "builtin":
            raise AwError("source_profile_binding_mismatch")
    return tuple(documents[name] for name in sorted(documents))


def verify_capabilities(source: ProfileSource, descriptor: dict) -> None:
    if binding := descriptor.get("instrumentation_profiles"):
        for key in ("events", "failure_actions"):
            if {event: set(effects) for event, effects in binding[key].items()} != {
                event: set(effects)
                for event, effects in source._capabilities.get(key, {}).items()
            }:
                raise AwError("source_profile_binding_mismatch")
