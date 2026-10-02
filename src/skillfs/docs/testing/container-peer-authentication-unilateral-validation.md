# Container Peer Authentication Unilateral Validation

[中文版](container-peer-authentication-unilateral-validation_zh.md)

This is the tester-run plan for the SkillFS side of
[issue #2439](https://github.com/alibaba/anolisa/issues/2439). It validates the
container HMAC contract in a real Linux or Kubernetes environment without
waiting for an independently released agent-sec-core image.

The independent standard-library Python probe at
[`scripts/container-peer-auth-probe.py`](../../scripts/container-peer-auth-probe.py)
acts only as a control client or notify server. Passing this plan proves the
SkillFS implementation and cross-language wire contract. It does not prove
agent-sec-core configuration, reconciliation, activation decisions, or release
readiness; those still require bilateral integration and ACK evidence.

## 1. Freeze the test inputs

Create an evidence directory outside the repository and record the following
before changing the environment:

```bash
export C7_EVIDENCE=/var/tmp/skillfs-c7-evidence
install -d -m 0700 "$C7_EVIDENCE"
git rev-parse HEAD | tee "$C7_EVIDENCE/git-commit.txt"
rustc --version | tee "$C7_EVIDENCE/rustc.txt"
python3 --version | tee "$C7_EVIDENCE/python.txt"
uname -a | tee "$C7_EVIDENCE/uname.txt"
```

For Kubernetes, also record immutable cluster and image inputs:

```bash
kubectl version -o yaml >"$C7_EVIDENCE/kubernetes-version.yaml"
kubectl get nodes -o wide >"$C7_EVIDENCE/nodes.txt"
kubectl get nodes -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.status.nodeInfo.containerRuntimeVersion}{"\n"}{end}' \
  >"$C7_EVIDENCE/container-runtime.txt"
kubectl -n "$C7_NAMESPACE" get pod "$C7_POD" -o yaml \
  >"$C7_EVIDENCE/pod.yaml"
kubectl -n "$C7_NAMESPACE" get pod "$C7_POD" \
  -o jsonpath='{range .status.containerStatuses[*]}{.name}{"\t"}{.imageID}{"\n"}{end}' \
  >"$C7_EVIDENCE/image-ids.txt"
```

Do not save the Secret object, raw secret, proof, or reusable credential in
the evidence directory.

## 2. Required topology

Use three containers with `shareProcessNamespace` absent or `false`:

| Container | Effective UID | Source | FUSE view | Runtime sockets | Auth file |
| --- | ---: | --- | --- | --- | --- |
| SkillFS | 0 for this test | read-write | mount creator | read-write | read-only |
| trusted probe | 0 for this test | same physical source at the same absolute path | optional | read-write | read-only |
| workload / attacker | different UID for normal I/O; repeat negative socket tests with UID 0 | absent | read-write, propagated | absent normally; read-only for negative tests | absent |

Use separate volumes for:

- the physical source;
- the propagated FUSE view;
- private runtime sockets; and
- the staged authentication file.

Kubernetes Secret projections use symlinked entries, while the production
loader deliberately opens the final component with `O_NOFOLLOW`. Therefore do
not pass a projected Secret path directly to SkillFS. Use an init container to
copy the projected key into a trusted `emptyDir`, set mode `0400` or `0600`,
and mount that staged file only into SkillFS and the trusted probe. This is an
acceptance requirement, not a relaxation of the no-follow rule.

The initial profile requires SkillFS and sec-core to use the same effective UID
for authenticated notify because SkillFS validates the sec-core-owned endpoint
and its parent against its own effective UID. Each auth file must also be owned
by its reader's effective UID. A future different-UID profile requires an
explicit endpoint ownership policy as well as separate private key copies that
contain the same raw bytes.

Mount or copy the probe script into the trusted probe container. Evidence can
use a private mounted directory at `$C7_EVIDENCE`; otherwise write inside the
probe container and retrieve each file with `kubectl cp`. Neither the probe nor
the evidence volume belongs in the workload container.

## 3. Prepare the fixture

Use these paths consistently in the trusted containers:

```bash
export C7_SOURCE=/var/lib/skillfs/source
export C7_MOUNT=/var/lib/skillfs/shared/mount
export C7_RUNTIME=/run/anolisa
export C7_SECRET=/run/anolisa/auth/skillfs.key
export C7_CONTROL=/run/anolisa/skillfs/control.sock
export C7_NOTIFY=/run/anolisa/probe/notify.sock
export C7_PROBE=/workspace/src/skillfs/scripts/container-peer-auth-probe.py
```

Generate one raw random key before creating the Kubernetes Secret:

```bash
umask 077
head -c 32 /dev/urandom >skillfs.key
test "$(wc -c <skillfs.key)" -eq 32
kubectl -n "$C7_NAMESPACE" create secret generic skillfs-c7-auth \
  --from-file=skillfs.key=skillfs.key
```

Seed a Flat-layout Skill with a visible snapshot. The live directory must also
contain `SKILL.md` so `skill.resolveLiveSource` can verify it:

```bash
install -d -m 0755 \
  "$C7_SOURCE/weather/.skill-meta/versions/v000001.snapshot"
printf '%s\n' '---' 'name: weather' 'description: C7 validation' '---' \
  >"$C7_SOURCE/weather/SKILL.md"
cp "$C7_SOURCE/weather/SKILL.md" \
  "$C7_SOURCE/weather/.skill-meta/versions/v000001.snapshot/SKILL.md"
printf '%s\n' '{"schemaVersion":1,"target":".skill-meta/versions/v000001.snapshot"}' \
  >"$C7_SOURCE/weather/.skill-meta/activation.json"
```

Start SkillFS in the foreground with the explicit container profile:

```bash
skillfs mount "$C7_SOURCE" "$C7_MOUNT" \
  --foreground --allow-other \
  --security --activation-mode file \
  --notify-socket "$C7_NOTIFY" \
  --notify-auth-key-file "$C7_SECRET" \
  --control-socket "$C7_CONTROL" \
  --trusted-peer-key-file "$C7_SECRET"
```

Before proceeding, require all of the following:

- the SkillFS process remains running;
- the mount appears in `/proc/self/mountinfo`;
- `skills/weather/SKILL.md` is readable through the workload's propagated
  view; and
- the control socket is mode `0600` in a private runtime directory; and
- the notify listener uses an owner-matched socket with no group/other bits
  under an owner-matched directory with no group/other bits (`0700` is the
  recommended default).

## 4. Positive control tests

Run these commands from the trusted probe container:

```bash
python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"ping"}' \
  | tee "$C7_EVIDENCE/control-ping.json"

python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"status"}' \
  | tee "$C7_EVIDENCE/control-status.json"

python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request "{\"schemaVersion\":\"1\",\"method\":\"skill.resolveLiveSource\",\"canonicalSkillDir\":\"$C7_SOURCE/weather\"}" \
  | tee "$C7_EVIDENCE/control-resolve.json"
```

Expected results:

- each process exits zero after mutual authentication;
- `ping` returns `ok=true` and `pong=true`;
- `status` retains the existing schema v1 business response;
- resolve returns `managed=true`, `skillId=weather`,
  `liveSkillDir=$C7_SOURCE/weather`, and `transport=shared_path`; and
- the printed nonce differs for every connection.

## 5. Negative control tests

### 5.1 Plain and wrong-key peers

Intentionally mount the runtime socket, but not the real auth file, into the
attacker container. Run it first as the normal workload UID and repeat as UID
0, matching SkillFS's effective UID:

```bash
python3 "$C7_PROBE" plain --socket "$C7_CONTROL"
umask 077
head -c 32 /dev/urandom >/tmp/wrong-skillfs.key
if python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret /tmp/wrong-skillfs.key \
  --request '{"schemaVersion":"1","method":"ping"}'; then
  echo 'FAIL: wrong-key peer was accepted' >&2
  exit 1
else
  echo 'PASS: wrong-key peer was rejected'
fi
```

The plain probe must print `PASS`. The wrong-key command must fail without a
business response. SkillFS must remain mounted and a subsequent trusted `ping`
must still succeed.

### 5.2 Replay, business tampering, and slow partial frames

```bash
python3 "$C7_PROBE" replay \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" --timeout 6
python3 "$C7_PROBE" tamper \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" --timeout 6
python3 "$C7_PROBE" slow \
  --socket "$C7_CONTROL" --interval 1 --bound 7
python3 "$C7_PROBE" control \
  --socket "$C7_CONTROL" --secret "$C7_SECRET" \
  --request '{"schemaVersion":"1","method":"ping"}'
```

The replay must observe a new nonce and receive no `auth.ok`. The tamper probe
must complete a valid handshake but receive no response after changing the raw
request without a matching tag. The slow sender must be disconnected within
seven seconds. The final trusted request must pass, proving the single-request
listener recovered.

### 5.3 Secret and configuration startup gates

For each row, start a fresh SkillFS process and assert non-zero exit before a
FUSE mount or socket is left behind:

| Case | Setup | Expected diagnostic |
| --- | --- | --- |
| missing | configured file is absent | cannot open authentication key |
| projected symlink | direct Kubernetes Secret entry | no-follow open failure |
| FIFO | mode `0600`, no writer | regular-file rejection without blocking |
| short | 31 raw bytes | must contain 32–4096 bytes |
| oversized | 4097 raw bytes | must contain 32–4096 bytes |
| permissive | mode `0640` or `0644` | group or other permissions rejected |
| wrong owner | owner differs from effective UID | effective-user ownership rejected |
| half-hardened | HMAC control plus notify socket, but no notify key | notify auth key required |
| mixed modes | both trusted peer executable and key | mutually exclusive |

After every failure, capture stderr, confirm no residual mount with `findmnt`,
and confirm that neither control nor notify socket remains.

## 6. Positive notify test

Start the independent notify server in the trusted probe container before
causing the mutation:

```bash
python3 "$C7_PROBE" notify-server \
  --socket "$C7_NOTIFY" --secret "$C7_SECRET" \
  --output "$C7_EVIDENCE/notify-request.json" \
  >"$C7_EVIDENCE/notify-probe.log" 2>&1 &
C7_NOTIFY_PID=$!
```

Wait for `READY`, then write through the workload-visible FUSE view:

```bash
grep -q '^READY:' "$C7_EVIDENCE/notify-probe.log"
date -u +%FT%TZ >"$C7_MOUNT/skills/weather/validation.txt"
wait "$C7_NOTIFY_PID"
python3 -m json.tool "$C7_EVIDENCE/notify-request.json"
```

Expected results:

- the probe prints `PASS: authenticated notify v2 recorded`;
- the request method is `skill_ledger.skillfs_notify_change`;
- `params.schemaVersion` is `2`;
- `params.canonicalSkillDir` is `$C7_SOURCE/weather`;
- `params.skillId` is `weather` and the changed path is relative; and
- normal FUSE I/O completes even if notification delivery later fails.

Startup reconcile may produce the first notify. If so, save it as separate
evidence, restart the one-shot probe, then perform the workload mutation.

## 7. Negative notify server proof

Start the probe with an intentionally invalid server proof, then trigger
another FUSE mutation:

```bash
python3 "$C7_PROBE" notify-server \
  --socket "$C7_NOTIFY" --secret "$C7_SECRET" \
  --output "$C7_EVIDENCE/notify-must-not-exist.json" \
  --wrong-server-proof \
  >"$C7_EVIDENCE/notify-wrong-server.log" 2>&1 &
C7_BAD_NOTIFY_PID=$!
date -u +%FT%TZ >"$C7_MOUNT/skills/weather/wrong-server-proof.txt"
wait "$C7_BAD_NOTIFY_PID"
test ! -e "$C7_EVIDENCE/notify-must-not-exist.json"
```

The probe must report that no business frame followed the wrong proof. SkillFS
must log an authentication failure, keep serving FUSE I/O, and leave activation
mapping unchanged.

## 8. Namespace and volume isolation

From the workload container, record and assert:

```bash
test ! -e "$C7_SOURCE"
test ! -e "$C7_SECRET"
test ! -e "$C7_CONTROL"
test ! -e "$C7_NOTIFY"
cat "$C7_MOUNT/skills/weather/SKILL.md" >/dev/null
```

Record PID and mount namespace identifiers from each container and require the
SkillFS and trusted probe values to differ. Do not enable a shared PID
namespace to make executable authentication work.

For the deliberate attacker case, mount only the runtime volume and repeat the
plain and wrong-key tests. Seeing the socket must not grant authority.

## 9. Restart and shutdown

1. Record the SkillFS container restart count and a successful nonce.
2. Terminate PID 1 in the SkillFS sidecar and wait for kubelet to restart it.
3. Require the mount probe to pass again and issue another authenticated
   `ping`; its nonce must be fresh.
4. Restart the trusted notify probe and require a new mutation to authenticate
   and deliver.
5. Start the slow control probe, terminate SkillFS, and require shutdown and
   unmount to finish within the configured handshake bound plus the normal
   container termination grace period.
6. Delete the Pod normally, wait for termination, and recreate it on the same
   node. Startup must not report a residual FUSE mount or stale socket.

Save pre/post restart counts, Pod events, SkillFS logs, probe output, and mount
probe output.

## 10. Credential disclosure check

Collect SkillFS and probe logs, then run a comparison inside a trusted
container without printing the credential encodings:

```bash
python3 - "$C7_SECRET" "$C7_EVIDENCE/combined.log" <<'PY'
import base64
import pathlib
import sys

secret = pathlib.Path(sys.argv[1]).read_bytes()
logs = pathlib.Path(sys.argv[2]).read_bytes()
markers = (secret, secret.hex().encode(), base64.b64encode(secret))
if any(marker in logs for marker in markers):
    raise SystemExit("FAIL: credential material appears in logs")
print("PASS: raw, hex, and Base64 credential material absent from logs")
PY
```

Also inspect JSON responses and protocol-event output. They may identify the
authentication mode, but must contain no secret, proof, or reusable credential.

## 11. Acceptance record

Mark each item `PASS`, `FAIL`, or `NOT RUN` with an evidence filename:

| ID | Required result |
| --- | --- |
| C7-U01 | real FUSE mount and propagated workload read pass |
| C7-U02 | authenticated ping, status, and shared-path resolve pass |
| C7-U03 | nonce differs; replay and modified business frames are rejected |
| C7-U04 | plain, wrong-key, and same-UID/no-secret peers are rejected |
| C7-U05 | slow partial authentication is bounded and listener recovers |
| C7-U06 | all secret/config startup gates fail before mounting |
| C7-U07 | authenticated notify v2 is received with unchanged business schema |
| C7-U08 | invalid notify server proof blocks the business request |
| C7-U09 | workload cannot see source, Secret, or private sockets |
| C7-U10 | SkillFS and probe restart paths reauthenticate with fresh challenges |
| C7-U11 | graceful deletion leaves no residual FUSE mount or socket |
| C7-U12 | logs, responses, and events disclose no credential material |

Any failed required row blocks unilateral acceptance. `NOT RUN` is allowed only
when the environment cannot supply the required FUSE, privilege, propagation,
or namespace feature, and the exact blocker is recorded.

## 12. Interpretation

A full pass is sufficient to ask sec-core maintainers to implement or review
against the frozen contract in
[`container-peer-authentication.md`](../design/container-peer-authentication.md).
It is not sufficient to close #2439. Closure additionally requires real
agent-sec-core resolver, notify, activation, failure, and restart integration in
separate containers, followed by the focused ACK record.
