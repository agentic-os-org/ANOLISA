"""Request decisions for the AgentSecCore secret gateway.

Stdlib only and free of I/O so the same code runs inside the standalone
mitmdump binary and under plain Python tests. Error messages never contain
fake or real credential values.
"""

from __future__ import annotations

import base64
import binascii
import ipaddress
import json
import re
from dataclasses import dataclass, field
from typing import Callable
from urllib.parse import unquote

PLAN_FORMAT = "agent-sec-gateway-plan/v2"
MANIFEST_VERSION = 1
SECRETS_VERSION = 2

MAX_PLAN_BYTES = 1024 * 1024
MAX_SECRETS_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 64 * 1024
MAX_VALUE_BYTES = 512
MAX_LISTENERS = 64
MAX_RULES = 256
MAX_CREDENTIALS = 64
MAX_ENTRIES = 4096
MAX_REQUEST_BYTES = 64 * 1024

FORMAT_AUTO = "auto"
FORMAT_BASIC = "basic"
METHODS = ("GET", "HEAD", "POST", "PUT", "PATCH", "DELETE")

ACTION_FORWARD = "forward"
ACTION_INJECT = "inject"
ACTION_REJECT = "reject"

_DNS_LABEL = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z")
_FILE_NAME = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}\Z")
_HEADER_NAME = re.compile(r"[!#$%&'*+.^_`|~A-Za-z0-9-]{1,128}\Z")
_CONTROL_HEADERS = frozenset(
    {
        "host",
        "connection",
        "proxy-connection",
        "keep-alive",
        "upgrade",
        "te",
        "trailer",
        "transfer-encoding",
        "content-length",
        "content-type",
        "content-encoding",
        "accept-encoding",
        "expect",
        "proxy-authorization",
        "proxy-authenticate",
    }
)
_ENCODED_DELIMITER = re.compile(r"%(?:2f|5c|2e)", re.IGNORECASE)


class LoadError(Exception):
    """A manifest, plan or secrets document is unusable."""


@dataclass(frozen=True)
class Rule:
    host: str
    path: re.Pattern[str] | None
    methods: frozenset[str]
    credential_id: str
    header: str
    format: str
    response_rewrite: bool


@dataclass(frozen=True)
class Plan:
    scope_id: str
    credentials: frozenset[str]
    rules: tuple[Rule, ...]


@dataclass(frozen=True)
class Entry:
    fake: str = field(repr=False)
    credential: str
    scope_ids: frozenset[str]
    real: str = field(repr=False)


@dataclass(frozen=True)
class ManagedFake:
    """Identity without Scope authorization or secret material."""

    credential: str


@dataclass(frozen=True)
class Snapshot:
    generation: int
    listeners: dict[int, Plan]
    live: dict[str, Entry] = field(repr=False)
    retired: dict[str, ManagedFake] = field(repr=False)
    unavailable: dict[str, ManagedFake] = field(repr=False)
    reals: tuple[str, ...] = field(repr=False)


@dataclass(frozen=True)
class RequestFacts:
    """What the addon observed; `path` includes the query string."""

    port: int
    tls: bool
    sni: str | None
    authority: str | None
    # Absolute-form request target authority; None for origin-form requests.
    target_authority: str | None
    method: str
    path: str
    headers: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class Decision:
    action: str
    code: str
    scope_id: str | None = None
    rule: int | None = None
    response_rewrite: bool | None = None
    header: str | None = None
    header_value: str | None = field(default=None, repr=False)
    # Restore the complete injected header value and bare reals before delivery.
    replacements: tuple[tuple[str, str], ...] = field(default=(), repr=False)


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise LoadError(message)


def _object(value: object, keys: set[str], what: str) -> dict[str, object]:
    _require(isinstance(value, dict), f"{what} must be an object")
    assert isinstance(value, dict)
    _require(set(value) == keys, f"{what} has missing or unknown fields")
    return value


def _string(value: object, what: str) -> str:
    _require(isinstance(value, str), f"{what} must be a string")
    assert isinstance(value, str)
    _require(
        not any(0xD800 <= ord(char) <= 0xDFFF for char in value),
        f"{what} must contain valid Unicode scalar values",
    )
    return value


def _integer(value: object, what: str) -> int:
    _require(
        isinstance(value, int) and not isinstance(value, bool),
        f"{what} must be an integer",
    )
    assert isinstance(value, int)
    return value


def _list(value: object, what: str) -> list[object]:
    _require(isinstance(value, list), f"{what} must be a list")
    assert isinstance(value, list)
    return value


def _dns_label(value: object, what: str) -> str:
    text = _string(value, what)
    _require(
        _DNS_LABEL.match(text) is not None, f"{what} must be a lowercase DNS label"
    )
    return text


def _token_value(value: object, what: str) -> str:
    text = _string(value, what)
    _require(
        0 < len(text) <= MAX_VALUE_BYTES and all("!" <= char <= "~" for char in text),
        f"{what} must be 1..=512 visible ASCII characters",
    )
    return text


def _host(value: object, what: str) -> str:
    host = _string(value, what)
    labels = host.split(".")
    _require(
        len(host) <= 253
        and len(labels) >= 2
        and all(_DNS_LABEL.match(x) for x in labels),
        f"{what} must be an exact lowercase DNS name",
    )
    try:
        ipaddress.ip_address(host)
    except ValueError:
        return host
    raise LoadError(f"{what} must be a DNS name, not an IP literal")


def compile_path_pattern(pattern: str) -> re.Pattern[str]:
    """Compile Cedar `like`: `*` spans slashes; `\\*` is a literal star."""
    _require(
        pattern.startswith("/")
        and 0 < len(pattern.encode()) <= 512
        and not any(_is_control(c) for c in pattern),
        "path pattern must be absolute and 1..=512 bytes without control characters",
    )
    parts = []
    index = 0
    while index < len(pattern):
        char = pattern[index]
        if char == "\\":
            index += 1
            _require(
                index < len(pattern) and pattern[index] in ("*", "\\"),
                "path pattern has an invalid escape",
            )
            parts.append(re.escape(pattern[index]))
        elif char == "*":
            # Adjacent wildcards have no separate pathglob meaning.
            if not parts or parts[-1] != ".*":
                parts.append(".*")
        else:
            parts.append(re.escape(char))
        index += 1
    chunks = "".join(parts).split(".*")
    expression = chunks[0]
    if len(chunks) > 1:
        # Commit intermediate wildcard matches to avoid exponential backtracking.
        expression += "".join(f"(?>.*?{chunk})" for chunk in chunks[1:-1])
        expression += f".*{chunks[-1]}"
    return re.compile(expression + r"\Z", re.DOTALL)


def _is_control(char: str) -> bool:
    """Unicode general category Cc, matching Rust `char::is_control`."""
    return ord(char) < 0x20 or 0x7F <= ord(char) <= 0x9F


def parse_plan(document: object) -> Plan:
    plan = _object(
        document,
        {"format", "scopeId", "policies", "credentials", "rules"},
        "plan",
    )
    _require(plan["format"] == PLAN_FORMAT, "plan format is not supported")
    agent = _dns_label(plan["scopeId"], "plan scope id")
    policies: dict[str, int] = {}
    policy_items = _list(plan["policies"], "policies")
    _require(0 < len(policy_items) <= 32, "plan requires 1..32 policy sources")
    for item in policy_items:
        origin = _object(item, {"id", "revision"}, "policy source")
        policy_id = _resource_id(origin["id"], "policy source id")
        revision = _revision(origin["revision"], "policy source revision")
        _require(
            not policies or next(reversed(policies)) < policy_id,
            "policy sources must be sorted and unique",
        )
        policies[policy_id] = revision
    credentials: set[str] = set()
    credential_items = _list(plan["credentials"], "plan credentials")
    _require(len(credential_items) <= MAX_CREDENTIALS, "plan has too many credentials")
    for item in credential_items:
        credential = _dns_label(item, "plan credential id")
        _require(credential not in credentials, "plan credentials must be unique")
        credentials.add(credential)
    rules = []
    locations: set[tuple[str, int]] = set()
    rule_items = _list(plan["rules"], "plan rules")
    _require(len(rule_items) <= MAX_RULES, "plan has too many rules")
    for position, item in enumerate(rule_items):
        rule = _object(
            item,
            {"source", "match", "header", "format", "responseRewrite", "because"},
            f"plan rule {position}",
        )
        origin = _object(
            rule["source"], {"policyId", "policyRevision", "ruleIndex"}, "rule source"
        )
        policy_id = _resource_id(origin["policyId"], "rule source policy")
        revision = _revision(origin["policyRevision"], "rule source revision")
        rule_index = _integer(origin["ruleIndex"], "rule source index")
        location = (policy_id, rule_index)
        _require(
            policies.get(policy_id) == revision
            and 0 <= rule_index < 1024
            and location not in locations,
            "rule source is not a unique declared policy location",
        )
        locations.add(location)
        rules.append(_parse_rule(rule, position, credentials))
    return Plan(agent, frozenset(credentials), tuple(rules))


def _resource_id(value: object, what: str) -> str:
    text = _string(value, what)
    _require(
        re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", text) is not None,
        f"{what} must be a bounded resource identifier",
    )
    return text


def _revision(value: object, what: str) -> int:
    revision = _integer(value, what)
    _require(1 <= revision <= 0xFFFFFFFF, f"{what} must be a positive u32 revision")
    return revision


def _parse_rule(item: object, position: int, credentials: set[str]) -> Rule:
    what = f"plan rule {position}"
    rule = _object(
        item,
        {"source", "match", "header", "format", "responseRewrite", "because"},
        what,
    )
    _require(
        isinstance(rule["responseRewrite"], bool), f"{what} rewrite must be boolean"
    )
    header = _string(rule["header"], f"{what} header")
    _require(
        _HEADER_NAME.fullmatch(header) is not None
        and header.lower() not in _CONTROL_HEADERS,
        f"{what} header must be a legal non-control header name",
    )
    format_text = _string(rule["format"], f"{what} format")
    _require(
        format_text in (FORMAT_AUTO, FORMAT_BASIC)
        or (
            0 < len(format_text.encode()) <= 512
            and format_text.count("{fake}") == 1
            and "{" not in format_text.replace("{fake}", "")
            and "}" not in format_text.replace("{fake}", "")
            and not any(_is_control(c) for c in format_text)
        ),
        f"{what} format must be auto, basic, or a single-placeholder template",
    )
    because = _string(rule["because"], what)
    _require(
        0 < len(because.encode()) <= 256 and not any(_is_control(c) for c in because),
        f"{what} because must be 1..=256 bytes without control characters",
    )
    match = _object(
        rule["match"], {"scheme", "host", "path", "methods", "credentialId"}, what
    )
    _require(match["scheme"] == "https", f"{what} scheme must be https")
    methods = [_string(x, f"{what} method") for x in _list(match["methods"], "methods")]
    _require(all(m in METHODS for m in methods), f"{what} method is not supported")
    _require(
        bool(methods)
        and all(
            METHODS.index(a) < METHODS.index(b) for a, b in zip(methods, methods[1:])
        ),
        f"{what} methods must be non-empty, sorted and unique",
    )
    credential = _dns_label(match["credentialId"], f"{what} credential")
    _require(credential in credentials, f"{what} names a credential outside the plan")
    path = None
    if match["path"] is not None:
        path_match = _object(match["path"], {"kind", "value"}, f"{what} path")
        value = _string(path_match["value"], f"{what} path value")
        _require(
            0 < len(value.encode()) <= 512 and not any(_is_control(c) for c in value),
            f"{what} path value is invalid",
        )
        if path_match["kind"] == "eq":
            _require(_canonical_path(value) == value, f"{what} path must be canonical")
            path = re.compile(re.escape(value) + r"\Z")
        else:
            _require(path_match["kind"] == "like", f"{what} path kind is unsupported")
            path = compile_path_pattern(value)
    rewrite = rule["responseRewrite"]
    assert isinstance(rewrite, bool)
    return Rule(
        host=_host(match["host"], f"{what} host"),
        path=path,
        methods=frozenset(methods),
        credential_id=credential,
        header=header,
        format=format_text,
        response_rewrite=rewrite,
    )


def parse_secrets(
    document: object,
) -> tuple[int, dict[str, Entry], dict[str, ManagedFake], dict[str, ManagedFake]]:
    secrets = _object(
        document,
        {"version", "generation", "entries", "retired", "unavailable"},
        "secrets",
    )
    _require(
        _integer(secrets["version"], "secrets version") == SECRETS_VERSION,
        "secrets version is not supported",
    )
    generation = _integer(secrets["generation"], "secrets generation")
    _require(
        1 <= generation <= 0xFFFFFFFFFFFFFFFF,
        "secrets generation must be a positive u64",
    )
    live: dict[str, Entry] = {}
    retired: dict[str, ManagedFake] = {}
    unavailable: dict[str, ManagedFake] = {}
    entries = _list(secrets["entries"], "secrets entries")
    tombstones = _list(secrets["retired"], "secrets retired")
    unselected = _list(secrets["unavailable"], "secrets unavailable")
    _require(
        len(entries) + len(tombstones) + len(unselected) <= MAX_ENTRIES,
        "secrets has too many entries",
    )
    per_scope: set[tuple[str, str]] = set()
    credential_ids: set[str] = set()
    for item in entries:
        entry = _object(item, {"fake", "credential", "scopeIds", "real"}, "entry")
        scopes = [
            _dns_label(s, "entry scope") for s in _list(entry["scopeIds"], "scopes")
        ]
        _require(
            0 < len(scopes) <= MAX_LISTENERS and len(set(scopes)) == len(scopes),
            "entry scopes must be a bounded non-empty unique set",
        )
        parsed = Entry(
            fake=_token_value(entry["fake"], "entry fake"),
            credential=_dns_label(entry["credential"], "entry credential"),
            scope_ids=frozenset(scopes),
            real=_token_value(entry["real"], "entry real"),
        )
        _require(parsed.fake not in live, "secrets fakes must be unique")
        _require(
            parsed.credential not in credential_ids, "secrets pair ids must be unique"
        )
        credential_ids.add(parsed.credential)
        _require(
            len(parsed.real) == len(parsed.fake),
            "real and fake must have equal lengths",
        )
        # Match the Store's pure validator: prefixes and punctuation count too.
        mismatches = sum(a != b for a, b in zip(parsed.real, parsed.fake))
        _require(
            mismatches * 5 >= len(parsed.real) * 4,
            "at least 80% of all real and fake positions must differ",
        )
        # A shared fake has several memberships, never several aliases in one Scope.
        memberships = {(scope, parsed.real) for scope in scopes}
        _require(
            not per_scope & memberships, "a scope maps one real value to several fakes"
        )
        per_scope.update(memberships)
        live[parsed.fake] = parsed
    known = set(live)
    for items, destination in ((tombstones, retired), (unselected, unavailable)):
        for item in items:
            identity = _object(item, {"fake", "credential"}, "managed fake")
            fake = _token_value(identity["fake"], "managed fake value")
            _require(fake not in known, "secrets fakes must be unique")
            known.add(fake)
            credential = _dns_label(identity["credential"], "managed fake credential")
            _require(
                credential not in credential_ids, "secrets pair ids must be unique"
            )
            credential_ids.add(credential)
            destination[fake] = ManagedFake(credential=credential)
    reals = {entry.real for entry in live.values()}
    _require(not reals & known, "a real value equals a fake")
    return generation, live, retired, unavailable


def _unique_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    keys = [key for key, _ in pairs]
    if len(set(keys)) != len(keys):
        raise ValueError("duplicate key")
    return dict(pairs)


def _decode_json(raw: bytes, what: str) -> object:
    """Reject duplicate keys and bounded-document decode failures without quoting input."""
    limit = {
        "manifest": MAX_MANIFEST_BYTES,
        "plan": MAX_PLAN_BYTES,
        "secrets": MAX_SECRETS_BYTES,
    }[what]
    _require(len(raw) <= limit, f"{what} exceeds its size limit")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_keys)
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise LoadError(f"{what} is not valid JSON") from None


def build_snapshot(manifest_raw: bytes, read: Callable[[str, int], bytes]) -> Snapshot:
    """Loads one complete generation; `read(name, limit)` returns a runtime file."""
    manifest = _object(
        _decode_json(manifest_raw, "manifest"),
        {"version", "generation", "secrets", "listeners"},
        "manifest",
    )
    _require(
        _integer(manifest["version"], "manifest version") == MANIFEST_VERSION,
        "manifest version is not supported",
    )
    generation = _integer(manifest["generation"], "manifest generation")
    _require(
        1 <= generation <= 0xFFFFFFFFFFFFFFFF,
        "manifest generation must be a positive u64",
    )
    secrets_name = _file_name(manifest["secrets"], "manifest secrets")
    secrets_generation, live, retired, unavailable = parse_secrets(
        _decode_json(read(secrets_name, MAX_SECRETS_BYTES), "secrets")
    )
    _require(
        secrets_generation == generation, "secrets generation differs from manifest"
    )
    listeners: dict[int, Plan] = {}
    agents: set[str] = set()
    items = _list(manifest["listeners"], "manifest listeners")
    _require(len(items) <= MAX_LISTENERS, "manifest has too many listeners")
    for item in items:
        listener = _object(item, {"port", "scopeId", "plan"}, "listener")
        port = _integer(listener["port"], "listener port")
        _require(
            0 < port < 65536 and port not in listeners, "listener ports must be unique"
        )
        agent = _dns_label(listener["scopeId"], "listener agent")
        _require(agent not in agents, "listener agents must be unique")
        agents.add(agent)
        plan_name = _file_name(listener["plan"], "listener plan")
        plan = parse_plan(_decode_json(read(plan_name, MAX_PLAN_BYTES), "plan"))
        _require(plan.scope_id == agent, "plan scope differs from listener scope")
        issued = {e.credential for e in live.values() if agent in e.scope_ids}
        _require(
            plan.credentials <= issued, "plan credential has no fake for its agent"
        )
        listeners[port] = plan
    reals = tuple(
        sorted({e.real for e in live.values()}, key=lambda real: (-len(real), real))
    )
    return Snapshot(generation, listeners, live, retired, unavailable, reals)


def _file_name(value: object, what: str) -> str:
    name = _string(value, what)
    _require(_FILE_NAME.match(name) is not None, f"{what} must be a plain file name")
    return name


def _split_authority(authority: str) -> tuple[str, int | None] | None:
    if len(authority) > 260 or any(c.isspace() or c in "/@?#\\" for c in authority):
        return None
    authority = authority.lower()
    if authority.startswith("["):
        host, bracket, rest = authority[1:].partition("]")
        if not bracket or (rest and not rest.startswith(":")):
            return None
        try:
            ipaddress.IPv6Address(host)
        except ValueError:
            return None
        port_text = rest[1:]
        has_port = bool(rest)
    else:
        host, separator, port_text = authority.partition(":")
        host = host.removesuffix(".")
        if not host or not all(
            _DNS_LABEL.fullmatch(label) for label in host.split(".")
        ):
            return None
        has_port = bool(separator)
    if not has_port:
        return host, None
    if not re.fullmatch(r"[0-9]{1,5}", port_text) or not 0 < int(port_text) < 65536:
        return None
    return host, int(port_text)


def _canonical_path(target: str) -> str | None:
    """Return an unambiguous path; queries are never used for rule matching."""
    path = target.partition("?")[0]
    if (
        not path.startswith("/")
        or "\\" in path
        or "#" in target
        or "//" in path
        or _ENCODED_DELIMITER.search(path)
        or any(c.isspace() or _is_control(c) for c in path)
        or re.search(r"%(?![0-9A-Fa-f]{2})", path)
    ):
        return None
    if any(segment in (".", "..") for segment in path.split("/")[1:]):
        return None
    return path


@dataclass(frozen=True)
class _Basic:
    scheme_word: str
    username: str = field(repr=False)
    password: str = field(repr=False)


def _parse_basic(value: str) -> _Basic | str | None:
    """Called only when a loaded plan explicitly enables Basic recognition."""
    words = value.strip().split(None, 1)
    if not words or words[0].lower() != "basic":
        return None
    if len(words) != 2:
        return "malformed_basic"
    try:
        decoded = base64.b64decode(words[1], validate=True).decode("utf-8")
    except (ValueError, binascii.Error, UnicodeDecodeError):
        return "malformed_basic"
    username, colon, password = decoded.partition(":")
    if not colon or any(_is_control(c) for c in decoded):
        return "malformed_basic"
    return _Basic(words[0], username, password)


def _fake_occurrences(value: str, fake: str) -> int:
    first = value.find(fake)
    return 0 if first < 0 else 1 + int(value.find(fake, first + 1) >= 0)


def _format_value(
    rule: Rule, value: str, entry: Entry, basic: _Basic | None
) -> str | None:
    if rule.format == FORMAT_BASIC:
        if basic is None or basic.password != entry.fake:
            return None
        encoded = base64.b64encode(f"{basic.username}:{entry.real}".encode()).decode()
        return f"{basic.scheme_word} {encoded}"
    if rule.format == FORMAT_AUTO:
        return (
            value.replace(entry.fake, entry.real, 1)
            if _fake_occurrences(value, entry.fake) == 1
            else None
        )
    if value != rule.format.replace("{fake}", entry.fake):
        return None
    return rule.format.replace("{fake}", entry.real)


def _contains_real(
    snapshot: Snapshot, facts: RequestFacts, extra: tuple[str, ...]
) -> bool:
    texts = [
        facts.path,
        unquote(facts.path),
        facts.authority or "",
        facts.target_authority or "",
        facts.sni or "",
        *extra,
    ]
    texts.extend(text for header in facts.headers for text in header)
    for real in snapshot.reals:
        if any(real in text for text in texts):
            return True
    return False


def _reject(code: str, agent: str | None = None) -> Decision:
    return Decision(ACTION_REJECT, code, scope_id=agent)


def decide_request(snapshot: Snapshot | None, facts: RequestFacts) -> Decision:
    """Validate original request facts, then require one unambiguous injection."""
    plan = snapshot.listeners.get(facts.port) if snapshot else None
    if snapshot is None or plan is None:
        return _reject("gateway_not_ready")
    agent = plan.scope_id
    if (
        len(facts.headers) > 256
        or sum(len(k.encode()) + len(v.encode()) for k, v in facts.headers)
        + sum(
            len(text.encode())
            for text in (
                facts.path,
                facts.method,
                facts.authority or "",
                facts.target_authority or "",
                facts.sni or "",
            )
        )
        > MAX_REQUEST_BYTES
    ):
        return _reject("request_too_large", agent)
    formats = {
        (r.header.lower(), r.format)
        for listener in snapshot.listeners.values()
        for r in listener.rules
    }
    related = {header for header, _ in formats} | {"host", ":authority"}
    headers: dict[str, str] = {}
    basics: dict[str, _Basic] = {}
    observed_basics: list[_Basic] = []
    basic_enabled = any(format_text == FORMAT_BASIC for _, format_text in formats)
    decoded = []
    for name, value in facts.headers:
        name = name.lower()
        if name in related and name in headers:
            return _reject("duplicate_header", agent)
        headers[name] = value
        if basic_enabled:
            basic = _parse_basic(value)
            if isinstance(basic, str):
                # An explicit Basic header cannot silently fall back to auto.
                if (name, FORMAT_BASIC) in formats:
                    return _reject(basic, agent)
            elif basic is not None:
                basics[name] = basic
                observed_basics.append(basic)
                decoded.extend((basic.username, basic.password))
    if _contains_real(snapshot, facts, tuple(decoded)):
        return _reject("real_credential_exposed", agent)

    known = set(snapshot.live) | set(snapshot.retired) | set(snapshot.unavailable)
    supplied: set[str] = set()
    occurrences = 0
    # Routing and request targets can never be injection locations.
    routing = (
        facts.authority or "",
        facts.target_authority or "",
        facts.sni or "",
        facts.path,
        unquote(facts.path),
        facts.method,
    )
    misplaced = {fake for fake in known if any(fake in text for text in routing)}
    texts = [text for header in facts.headers for text in header]
    for basic in observed_basics:
        if any(fake in basic.username for fake in known):
            return _reject("fake_in_username", agent)
        texts.append(basic.password)
    for fake in known:
        count = sum(_fake_occurrences(text, fake) for text in texts)
        if count:
            supplied.add(fake)
            occurrences = min(2, occurrences + count)
    if (supplied | misplaced) & snapshot.retired.keys():
        return _reject("retired_credential", agent)
    if misplaced or supplied & snapshot.unavailable.keys():
        # Unavailable is generation-local, not retirement, and carries no grant.
        return _reject("credential_not_allowed", agent)
    if any(agent not in snapshot.live[fake].scope_ids for fake in supplied):
        return _reject("subject_mismatch", agent)
    if occurrences > 1:
        return _reject("ambiguous_credential", agent)
    entry = snapshot.live[next(iter(supplied))] if supplied else None
    if not facts.tls:
        return _reject("plaintext_credential", agent) if entry else _default(plan)

    parsed = _split_authority(facts.authority) if facts.authority else None
    if facts.authority is not None and parsed is None:
        return _reject("invalid_host", agent)
    if (
        parsed is not None
        and facts.sni is not None
        and facts.sni.lower().rstrip(".") != parsed[0]
    ):
        return _reject("sni_host_mismatch", agent)
    if facts.target_authority is not None:
        target = _split_authority(facts.target_authority)
        if (
            parsed is None
            or target is None
            or target[0] != parsed[0]
            or (target[1] or 443) != (parsed[1] or 443)
        ):
            return _reject("authority_mismatch", agent)
    if entry is None:
        return _default(plan)
    if parsed is None or not facts.sni:
        return _reject("sni_missing", agent)
    if any(name.lower() == "upgrade" for name, _ in facts.headers):
        # Upgraded protocols bypass response inspection.
        return _reject("upgrade_not_allowed", agent)
    host, port = parsed
    path = _canonical_path(facts.path)
    if port not in (None, 443) or path is None:
        return _reject("credential_not_allowed", agent)

    candidates: dict[tuple[str, str, str, str, str], tuple[int, Rule, str, str]] = {}
    for position, rule in enumerate(plan.rules):
        header = rule.header.lower()
        if (
            rule.host != host
            or facts.method not in rule.methods
            or entry.credential != rule.credential_id
            or header not in headers
            or (rule.path is not None and rule.path.fullmatch(path) is None)
        ):
            continue
        original = headers[header]
        value = _format_value(rule, original, entry, basics.get(header))
        if value is not None:
            # Different formats or rewrite modes are conflicts, not ordered priorities.
            key = (entry.credential, header, rule.format, rule.response_rewrite, value)
            candidates.setdefault(key, (position, rule, original, value))
    if not candidates:
        return _reject("credential_not_allowed", agent)
    if len(candidates) != 1:
        return _reject("ambiguous_injection", agent)
    position, rule, original, value = next(iter(candidates.values()))
    replacements = {value: original}
    if rule.format == FORMAT_BASIC:
        # Origins may echo only the Base64 payload instead of the scheme and payload.
        replacements[value.partition(" ")[2]] = original.strip().split(None, 1)[1]
    for live_entry in snapshot.live.values():
        if agent in live_entry.scope_ids:
            if (
                live_entry.real in replacements
                and replacements[live_entry.real] != live_entry.fake
            ):
                return _reject("ambiguous_injection", agent)
            replacements[live_entry.real] = live_entry.fake
    return Decision(
        ACTION_INJECT,
        "credential_injected",
        scope_id=agent,
        rule=position,
        response_rewrite=rule.response_rewrite,
        header=rule.header,
        header_value=value,
        replacements=tuple(replacements.items()),
    )


def _default(plan: Plan) -> Decision:
    return Decision(ACTION_FORWARD, "no_credential", scope_id=plan.scope_id)


def _compile_replacements(
    replacements: tuple[tuple[str, str], ...],
) -> tuple[dict[bytes, bytes], re.Pattern[bytes] | None]:
    mapping = {real.encode(): fake.encode() for real, fake in replacements}
    pattern = (
        re.compile(
            b"|".join(re.escape(s) for s in sorted(mapping, key=len, reverse=True))
        )
        if mapping
        else None
    )
    return mapping, pattern


def rewrite_bytes(body: bytes, replacements: tuple[tuple[str, str], ...]) -> bytes:
    """Replace longest original matches once; never scan replacement output."""
    mapping, pattern = _compile_replacements(replacements)
    if pattern is None:
        return body
    return pattern.sub(lambda match: mapping[match.group()], body)


class StreamingRewriter:
    """Rewrite once across chunks, retaining only undecided original suffixes."""

    def __init__(self, replacements: tuple[tuple[str, str], ...]) -> None:
        self.mapping, self.pattern = _compile_replacements(replacements)
        self.pending = b""
        self.prefixes: dict[bytes, list[int]] = {}
        for needle in self.mapping:
            prefix = [0] * len(needle)
            matched = 0
            for index in range(1, len(needle)):
                while matched and needle[index] != needle[matched]:
                    matched = prefix[matched - 1]
                if needle[index] == needle[matched]:
                    matched += 1
                prefix[index] = matched
            self.prefixes[needle] = prefix

    def __call__(self, chunk: bytes) -> bytes:
        if self.pattern is None:
            return chunk
        body = self.pending + chunk
        cutoff = len(body)
        if chunk:
            for needle, prefix in self.prefixes.items():
                kept = 0
                # A proper prefix can only occupy the final len(needle)-1 bytes.
                for byte in body[max(0, len(body) - len(needle) + 1) :]:
                    while kept and byte != needle[kept]:
                        kept = prefix[kept - 1]
                    if byte == needle[kept]:
                        kept += 1
                cutoff = min(cutoff, len(body) - kept)
        output = bytearray()
        start = 0
        for match in self.pattern.finditer(body):
            # A complete short match must wait if a longer prefix starts here.
            if match.start() >= cutoff:
                break
            output.extend(body[start : match.start()])
            output.extend(self.mapping[match.group()])
            start = match.end()
        # Leftmost matches consume overlapping partial candidates in full.
        end = max(start, cutoff)
        output.extend(body[start:end])
        self.pending = body[end:]
        return bytes(output)
