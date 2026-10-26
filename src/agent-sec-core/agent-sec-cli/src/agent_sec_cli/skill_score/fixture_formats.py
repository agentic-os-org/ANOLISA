"""Build the one binary fixture format currently needed, without extracting its members."""

import gzip
import io
import json
import re
import tarfile


def build_deb(specification: str) -> bytes:
    """Encode small text members as a valid Debian package, including adversarial member names."""
    value = json.loads(specification)
    if not isinstance(value, dict) or set(value) - {"package", "version", "architecture", "files"}:
        raise ValueError(
            "deb content must be JSON with package, version, architecture and files only"
        )
    version, architecture = value.get("version"), value.get("architecture")
    if not isinstance(version, str) or not re.fullmatch(r"[0-9][A-Za-z0-9.+:~\-]{0,63}", version):
        raise ValueError(
            "deb fixture requires an explicit version; use that value in task expectations"
        )
    if not isinstance(architecture, str) or architecture not in {"all", "amd64", "arm64"}:
        raise ValueError(
            "deb fixture requires explicit architecture=all/amd64/arm64; derive output from that input"
        )
    package, files = value.get("package", "score-fixture"), value.get("files", {})
    if not isinstance(package, str) or not re.fullmatch(r"[a-z0-9][a-z0-9+.-]{1,63}", package):
        raise ValueError("deb package needs a valid lowercase package name")
    if not isinstance(files, dict) or not 1 <= len(files) <= 128:
        raise ValueError("deb files must contain 1-128 text members")
    total = 0
    for name, content in files.items():
        if not isinstance(name, str) or not name or "\x00" in name or len(name) > 256:
            raise ValueError("invalid deb member name")
        if not isinstance(content, str):
            raise ValueError("deb members must contain text")
        total += len(content.encode())
    if total > 524288:
        raise ValueError("deb fixture exceeds 512 KiB of input data")

    def archive(members: dict[str, str]) -> bytes:
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.GNU_FORMAT) as tar:
            for name, content in sorted(members.items()):
                data = content.encode()
                entry = tarfile.TarInfo(name)
                entry.size, entry.mode, entry.mtime = len(data), 0o644, 0
                tar.addfile(entry, io.BytesIO(data))
        return gzip.compress(stream.getvalue(), mtime=0)

    control = (
        f"Package: {package}\nVersion: {version}\nArchitecture: {architecture}\n"
        "Maintainer: Fixture <fixture@example.invalid>\nDescription: Disposable test input\n"
    )
    result = bytearray(b"!<arch>\n")
    for name, data in (
        ("debian-binary", b"2.0\n"),
        ("control.tar.gz", archive({"control": control})),
        ("data.tar.gz", archive(files)),
    ):
        result.extend(f"{name + '/':<16}{0:<12}{0:<6}{0:<6}{100644:<8}{len(data):<10}`\n".encode())
        result.extend(data)
        if len(data) % 2:
            result.extend(b"\n")
    return bytes(result)
