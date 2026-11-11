"""Request decisions for the AgentSecCore secret gateway.

Stdlib only and free of I/O so the same code runs inside the standalone
mitmdump binary and under plain Python tests. Error messages never contain
fake or real credential values.
"""

from __future__ import annotations

import base64
import binascii
import json
import re
from dataclasses import dataclass, field
from typing import Callable
from urllib.parse import unquote

PLAN_FORMAT = "agent-sec-gateway-plan/v1"
MANIFEST_VERSION = 1
SECRETS_VERSION = 1

MAX_PLAN_BYTES = 1024 * 1024
MAX_SECRETS_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 64 * 1024
MAX_VALUE_BYTES = 512
MAX_LISTENERS = 64
MAX_RULES = 256
MAX_CREDENTIALS = 64

CARRIER_HEADER_TOKEN = "header_token"
CARRIER_BASIC_PASSWORD = "basic_password"
CARRIERS = frozenset({CARRIER_HEADER_TOKEN, CARRIER_BASIC_PASSWORD})
METHODS = ("GET", "HEAD", "POST", "PUT", "PATCH", "DELETE")

ACTION_FORWARD = "forward"
ACTION_INJECT = "inject"
ACTION_REJECT = "reject"

_DNS_LABEL = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z")
_FILE_NAME = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}\Z")
_LITERAL_SEGMENT = re.compile(r"[A-Za-z0-9._~-]+\Z")
_ENCODED_DELIMITER = re.compile(r"%(?:2f|5c|2e)", re.IGNORECASE)


class LoadError(Exception):
    """A manifest, plan or secrets document is unusable."""


@dataclass(frozen=True)
class Rule:
    host: str
    path: re.Pattern[str]
    methods: frozenset[str]
    credential_id: str
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
    scope_id: str
    real: str = field(repr=False)
    carriers: frozenset[str]


@dataclass(frozen=True)
class Retired:
    credential: str
    scope_id: str


@dataclass(frozen=True)
class Snapshot:
    generation: int
    listeners: dict[int, Plan]
    live: dict[str, Entry] = field(repr=False)
    retired: dict[str, Retired] = field(repr=False)
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
    authorization: str | None = field(default=None, repr=False)
    # Injected authorization and bare real values mapped back before delivery.
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
    return host


def compile_path_pattern(pattern: str) -> re.Pattern[str]:
    """`*` matches within one segment and `**` across segments."""
    _require(
        pattern.startswith("/") and len(pattern) <= 512, "path pattern must be absolute"
    )
    parts = []
    for segment in pattern[1:].split("/"):
        if segment == "**":
            parts.append(".*")
        elif segment == "*":
            parts.append("[^/]+")
        else:
            _require(
                _LITERAL_SEGMENT.match(segment) is not None
                and segment not in (".", ".."),
                "path pattern has an invalid segment",
            )
            parts.append(re.escape(segment))
    return re.compile("/" + "/".join(parts) + r"\Z")


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
    for item in _list(plan["policies"], "policies"):
        origin = _object(item, {"id", "revision"}, "policy source")
        policy_id = _resource_id(origin["id"], "policy source id")
        revision = _revision(origin["revision"], "policy source revision")
        _require(
            not policies or next(reversed(policies)) < policy_id,
            "policy sources must be sorted and unique",
        )
        policies[policy_id] = revision
    _require(0 < len(policies) <= 32, "plan requires 1..32 policy sources")
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
            {"source", "match", "responseRewrite", "because"},
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
    rule = _object(item, {"source", "match", "responseRewrite", "because"}, what)
    _require(
        isinstance(rule["responseRewrite"], bool), f"{what} rewrite must be boolean"
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
    rewrite = rule["responseRewrite"]
    assert isinstance(rewrite, bool)
    return Rule(
        host=_host(match["host"], f"{what} host"),
        path=compile_path_pattern(_string(match["path"], f"{what} path")),
        methods=frozenset(methods),
        credential_id=credential,
        response_rewrite=rewrite,
    )


def parse_secrets(document: object) -> tuple[int, dict[str, Entry], dict[str, Retired]]:
    secrets = _object(
        document, {"version", "generation", "entries", "retired"}, "secrets"
    )
    _require(secrets["version"] == SECRETS_VERSION, "secrets version is not supported")
    generation = _integer(secrets["generation"], "secrets generation")
    live: dict[str, Entry] = {}
    retired: dict[str, Retired] = {}
    for item in _list(secrets["entries"], "secrets entries"):
        entry = _object(
            item, {"fake", "credential", "scopeId", "real", "carriers"}, "entry"
        )
        carriers = [_string(x, "carrier") for x in _list(entry["carriers"], "carriers")]
        _require(
            bool(carriers) and set(carriers) <= CARRIERS, "entry carriers are invalid"
        )
        parsed = Entry(
            fake=_token_value(entry["fake"], "entry fake"),
            credential=_dns_label(entry["credential"], "entry credential"),
            scope_id=_dns_label(entry["scopeId"], "entry agent"),
            real=_token_value(entry["real"], "entry real"),
            carriers=frozenset(carriers),
        )
        _require(parsed.fake not in live, "secrets fakes must be unique")
        live[parsed.fake] = parsed
    for item in _list(secrets["retired"], "secrets retired"):
        tombstone = _object(item, {"fake", "credential", "scopeId"}, "tombstone")
        fake = _token_value(tombstone["fake"], "tombstone fake")
        _require(
            fake not in live and fake not in retired, "secrets fakes must be unique"
        )
        retired[fake] = Retired(
            credential=_dns_label(tombstone["credential"], "tombstone credential"),
            scope_id=_dns_label(tombstone["scopeId"], "tombstone agent"),
        )
    reals = {entry.real for entry in live.values()}
    _require(not reals & (set(live) | set(retired)), "a real value equals a fake")
    # Response rewriting maps each real back to exactly one fake per domain.
    per_domain = {(entry.scope_id, entry.real) for entry in live.values()}
    _require(
        len(per_domain) == len(live), "a domain maps one real value to several fakes"
    )
    return generation, live, retired


def _unique_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    keys = [key for key, _ in pairs]
    if len(set(keys)) != len(keys):
        raise ValueError("duplicate key")
    return dict(pairs)


def _decode_json(raw: bytes, what: str) -> object:
    """Rejects duplicate keys, as the Rust decoder of the same documents does."""
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_keys)
    except (UnicodeDecodeError, ValueError):
        raise LoadError(f"{what} is not valid JSON") from None


def build_snapshot(manifest_raw: bytes, read: Callable[[str, int], bytes]) -> Snapshot:
    """Loads one complete generation; `read(name, limit)` returns a runtime file."""
    manifest = _object(
        _decode_json(manifest_raw, "manifest"),
        {"version", "generation", "secrets", "listeners"},
        "manifest",
    )
    _require(
        manifest["version"] == MANIFEST_VERSION, "manifest version is not supported"
    )
    generation = _integer(manifest["generation"], "manifest generation")
    _require(generation >= 1, "manifest generation must be positive")
    secrets_name = _file_name(manifest["secrets"], "manifest secrets")
    secrets_generation, live, retired = parse_secrets(
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
        issued = {e.credential for e in live.values() if e.scope_id == agent}
        _require(
            plan.credentials <= issued, "plan credential has no fake for its agent"
        )
        listeners[port] = plan
    reals = tuple(sorted({e.real for e in live.values()}, key=len, reverse=True))
    return Snapshot(generation, listeners, live, retired, reals)


def _file_name(value: object, what: str) -> str:
    name = _string(value, what)
    _require(_FILE_NAME.match(name) is not None, f"{what} must be a plain file name")
    return name


def _split_authority(authority: str) -> tuple[str, int | None] | None:
    authority = authority.strip().lower()
    if authority.startswith("["):
        host, bracket, rest = authority[1:].partition("]")
        if not bracket or (rest and not rest.startswith(":")):
            return None
        port_text = rest[1:]
    else:
        host, _, port_text = authority.partition(":")
    if not host:
        return None
    if not port_text:
        return host.rstrip("."), None
    if not port_text.isdigit() or not 0 < int(port_text) < 65536:
        return None
    return host.rstrip("."), int(port_text)


def _canonical_path(target: str) -> str | None:
    """Returns the path for rule matching, or None when its meaning could differ upstream."""
    path = target.partition("?")[0].partition("#")[0]
    if not path.startswith("/") or "\\" in path or _ENCODED_DELIMITER.search(path):
        return None
    if any(segment in (".", "..") for segment in path.split("/")[1:]):
        return None
    return path


@dataclass(frozen=True)
class _Presented:
    token: str = field(repr=False)
    carrier: str
    scheme_word: str
    username: str = field(default="", repr=False)


def _parse_authorization(value: str) -> _Presented | str | None:
    """Returns the presented token, a reject code, or None for other schemes."""
    scheme_word, _, rest = value.strip().partition(" ")
    scheme = scheme_word.lower()
    if scheme in ("bearer", "token"):
        token = rest.strip()
        return _Presented(token, CARRIER_HEADER_TOKEN, scheme_word) if token else None
    if scheme != "basic":
        return None
    try:
        decoded = base64.b64decode(rest.strip(), validate=True).decode("utf-8")
    except (binascii.Error, UnicodeDecodeError):
        return "malformed_authorization"
    username, colon, password = decoded.partition(":")
    if not colon:
        return "malformed_authorization"
    return _Presented(password, CARRIER_BASIC_PASSWORD, scheme_word, username)


def _contains_real(
    snapshot: Snapshot, facts: RequestFacts, extra: tuple[str, ...]
) -> bool:
    texts = [facts.path, unquote(facts.path), *extra]
    texts.extend(value for _, value in facts.headers)
    for real in snapshot.reals:
        if any(real in text for text in texts):
            return True
    return False


def _reject(code: str, agent: str | None = None) -> Decision:
    return Decision(ACTION_REJECT, code, scope_id=agent)


def decide_request(snapshot: Snapshot | None, facts: RequestFacts) -> Decision:
    """Decides one request on an Agent listener; any uncertainty rejects."""
    plan = snapshot.listeners.get(facts.port) if snapshot else None
    if snapshot is None or plan is None:
        return _reject("gateway_not_ready")
    agent = plan.scope_id
    authorizations = [v for k, v in facts.headers if k.lower() == "authorization"]
    if len(authorizations) > 1:
        return _reject("duplicate_authorization", agent)
    presented = _parse_authorization(authorizations[0]) if authorizations else None
    if isinstance(presented, str):
        return _reject(presented, agent)
    decoded = (presented.username, presented.token) if presented else ()
    if _contains_real(snapshot, facts, decoded):
        return _reject("real_credential_exposed", agent)

    entry: Entry | None = None
    if presented is not None:
        if (
            presented.username in snapshot.live
            or presented.username in snapshot.retired
        ):
            return _reject("fake_in_username", agent)
        if presented.token in snapshot.retired:
            return _reject("retired_credential", agent)
        entry = snapshot.live.get(presented.token)
        if entry is not None:
            if entry.scope_id != agent:
                return _reject("subject_mismatch", agent)
            if presented.carrier not in entry.carriers:
                return _reject("carrier_not_allowed", agent)

    if not facts.tls:
        if entry is not None:
            return _reject("plaintext_credential", agent)
        return _default(plan)

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
        if parsed is None or target is None or target[0] != parsed[0]:
            # An absolute-form target can route to another virtual host behind the verified one.
            return _reject("authority_mismatch", agent)
    if entry is None:
        return _default(plan)
    assert presented is not None
    if parsed is None or facts.sni is None:
        return _reject("sni_missing", agent)
    if any(name.lower() == "upgrade" for name, _ in facts.headers):
        # Upgraded protocols bypass response inspection.
        return _reject("upgrade_not_allowed", agent)
    host, port = parsed
    path = _canonical_path(facts.path)
    matched = None
    if port in (None, 443) and path is not None:
        matched = next(
            (
                (position, rule)
                for position, rule in enumerate(plan.rules)
                if rule.host == host
                and facts.method in rule.methods
                and entry.credential == rule.credential_id
                and rule.path.match(path)
            ),
            None,
        )
    if matched is None:
        return _reject("credential_not_allowed", agent)
    # Decisions cite the published array, not the original authored rule position.
    position, rule = matched
    if presented.carrier == CARRIER_BASIC_PASSWORD:
        pair = f"{presented.username}:{entry.real}".encode()
        authorization = f"{presented.scheme_word} {base64.b64encode(pair).decode()}"
    else:
        authorization = f"{presented.scheme_word} {entry.real}"
    return Decision(
        ACTION_INJECT,
        "credential_injected",
        scope_id=agent,
        rule=position,
        response_rewrite=rule.response_rewrite,
        authorization=authorization,
        replacements=((authorization, authorizations[0]), (entry.real, entry.fake)),
    )


def _default(plan: Plan) -> Decision:
    return Decision(ACTION_FORWARD, "no_credential", scope_id=plan.scope_id)


def rewrite_bytes(body: bytes, replacements: tuple[tuple[str, str], ...]) -> bytes:
    for real, fake in replacements:
        body = body.replace(real.encode(), fake.encode())
    return body


class _StreamReplacement:
    def __init__(self, real: str, fake: str) -> None:
        self.real = real.encode()
        self.fake = fake.encode()
        self.pending = b""
        self.prefix = [0] * len(self.real)
        matched = 0
        for index in range(1, len(self.real)):
            while matched and self.real[index] != self.real[matched]:
                matched = self.prefix[matched - 1]
            if self.real[index] == self.real[matched]:
                matched += 1
            self.prefix[index] = matched

    def rewrite(self, chunk: bytes, final: bool) -> bytes:
        body = self.pending + chunk
        output = bytearray()
        start = 0
        while (position := body.find(self.real, start)) != -1:
            output.extend(body[start:position])
            output.extend(self.fake)
            start = position + len(self.real)
        kept = 0
        if not final:
            # Only an unmatched suffix can still become a cross-chunk match.
            for byte in body[max(start, len(body) - len(self.real) + 1) :]:
                while kept and byte != self.real[kept]:
                    kept = self.prefix[kept - 1]
                if byte == self.real[kept]:
                    kept += 1
        end = len(body) - kept
        output.extend(body[start:end])
        self.pending = body[end:]
        return bytes(output)


class StreamingRewriter:
    """Matches across chunks with the same ordered substitutions as rewrite_bytes."""

    def __init__(self, replacements: tuple[tuple[str, str], ...]) -> None:
        self.stages = tuple(
            _StreamReplacement(real, fake) for real, fake in replacements
        )

    def __call__(self, chunk: bytes) -> bytes:
        final = not chunk
        for stage in self.stages:
            chunk = stage.rewrite(chunk, final)
        return chunk
