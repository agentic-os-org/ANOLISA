"""Exact DNS and IP authorities shared by deployment readers and the model relay."""

from ipaddress import IPv4Address, IPv6Address
import re

LABEL = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?")


def authority(entry: str) -> tuple[str, int | None]:
    """Normalize a host grant, preserving the distinction between any and one port."""
    if not isinstance(entry, str) or not re.fullmatch(r"[A-Za-z0-9.:\[\]-]+", entry):
        raise ValueError("expected an exact HTTP host")
    if entry.startswith("["):
        host, close, suffix = entry[1:].partition("]")
        if not close or (suffix and not suffix.startswith(":")):
            raise ValueError("invalid bracketed HTTP host")
        host = str(IPv6Address(host))
        port = suffix[1:] if suffix else None
    elif entry.count(":") > 1:
        return str(IPv6Address(entry)), None
    else:
        host, separator, port_text = entry.partition(":")
        host = host.lower()
        if re.fullmatch(r"[0-9.]+", host):
            host = str(IPv4Address(host))
        else:
            labels = host.removesuffix(".")
            if len(labels) > 253 or not all(LABEL.fullmatch(label) for label in labels.split(".")):
                raise ValueError("invalid HTTP hostname")
        port = port_text if separator else None
    if port is None:
        return host, None
    if not port.isdecimal():
        raise ValueError("expected a decimal HTTP port")
    number = int(port.lstrip("0") or "0")
    if not 1 <= number <= 65535:
        raise ValueError("HTTP port must be in 1..65535")
    return host, number


def host_grants(entries: list[str]) -> set[tuple[str, int | None]]:
    if not isinstance(entries, list):
        raise ValueError("expected an HTTP host list")
    return {authority(entry) for entry in entries}


def allows_http(netloc: str, entries: list[str]) -> bool:
    grants = host_grants(entries)
    if not grants:
        return False
    try:
        host, port = authority(netloc)
    except ValueError:
        return False
    # Only the grant syntax permits an unbracketed IPv6 address.
    if ":" in host and not netloc.startswith("["):
        return False
    return (host, None) in grants or (host, port or 80) in grants
