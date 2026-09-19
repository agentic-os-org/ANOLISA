---
name: openclaw-file-guard
description: Run a native Linux end-to-end test for AgentSecCore, AgentSight, and OpenClaw file-deletion protection. Use when building the real ActPlane enforcer, binding a deletion policy to an OpenClaw gateway process tree, verifying EPERM and violation receipts, or checking that enforcement does not affect an unrelated host shell.
---

# Test OpenClaw File Guard

Run this test only on an isolated Linux host. Verify real BPF-LSM enforcement rather than treating a compiled policy or an `enforced` state as proof of protection.

## Safety boundaries

- Do not run against production workloads.
- Use the isolated paths and bpffs root below. The AgentSight port is fixed at `7396` because AgentSecCore V2 hardcodes it; ensure no production AgentSight is listening there before starting the test.
- Inspect and record any existing AgentSight services before stopping them. Restore only services that were running before the test.
- Do not reset, clean, or otherwise discard unrelated repository changes.
- Run the OpenClaw command through a local gateway child process, never a remote node, container, or unrelated executor.

## Prerequisites

Require a supported Linux kernel, root or `sudo`, BTF at `/sys/kernel/btf/vmlinux`, `CONFIG_BPF_SYSCALL=y`, `CONFIG_BPF_LSM=y`, and an enabled `bpf` LSM. Ensure `securityfs` and `bpffs` are mounted.

```bash
uname -a
test -r /sys/kernel/btf/vmlinux

if test -r /proc/config.gz; then
  zcat /proc/config.gz | rg 'CONFIG_BPF_SYSCALL|CONFIG_BPF_LSM'
fi

test -d /sys/kernel/security
mountpoint -q /sys/kernel/security || \
  sudo mount -t securityfs securityfs /sys/kernel/security
cat /sys/kernel/security/lsm | rg '(^|,)bpf(,|$)'

test -d /sys/fs/bpf
mountpoint -q /sys/fs/bpf || sudo mount -t bpf bpf /sys/fs/bpf
findmnt /sys/fs/bpf
```

Install the repository's documented build dependencies, including Rust, clang/LLVM, libbpf, libelf, OpenSSL, `bpftool`, `curl`, `jq`, `rg`, `findmnt`, `prlimit`, and OpenClaw. Confirm no conflicting test or production processes are active before continuing.

```bash
pgrep -af 'agentsight-enforcer|agentsight serve|agentsight trace' || true
ss -ltnp | rg ':7396\b' || true
systemctl is-active agentsight.service || true
systemctl is-active agentsight-enforcer.service || true
```

## Configure isolated resources

Set the repository paths for the local checkout. The AgentSight port must remain `7396` because AgentSecCore V2 hardcodes `http://127.0.0.1:7396/api`; only change the remaining values if they conflict with another isolated test.

```bash
export AGENTSIGHT_ROOT=<agentsight-repository>
export ASC_REPO_ROOT=<agent-sec-core-repository>
export ASC_V2_DIR="$ASC_REPO_ROOT/v2"
export OPENCLAW_USER=openclaw
# This user must exist and have OpenClaw configured with a valid model provider.

export AGENTSIGHT_E2E_PORT=7396
export AGENTSIGHT_E2E_BASE_URL="http://127.0.0.1:${AGENTSIGHT_E2E_PORT}"
export AGENTSIGHT_E2E_SOCKET=/run/agentsight-openclaw-e2e/enforcer.sock
export AGENTSIGHT_E2E_STATE=/tmp/agentsight-openclaw-e2e
export AGENTSIGHT_E2E_PIN_ROOT=/sys/fs/bpf/actplane-openclaw-e2e/v1
export AGENTSIGHT_E2E_PROTECTED=/tmp/oc-delete-guard
export ASC_E2E_DIR="${TMPDIR:-/tmp}/asc-openclaw-file-guard-${UID}"
export ASC_DAEMON_SOCKET="$ASC_E2E_DIR/asc-daemon.sock"

sudo install -d -m 0750 /run/agentsight-openclaw-e2e
sudo install -d -m 0700 /tmp/agentsight-openclaw-e2e
install -d -m 0700 "$ASC_E2E_DIR"
```

## Build and start AgentSight

Build the API server first, then overwrite the default enforcer with the real ActPlane build.

```bash
cd "$AGENTSIGHT_ROOT"
make build
make build-enforcer

stat target/release/agentsight target/release/agentsight-enforcer
strings target/release/agentsight-enforcer | rg 'agent-file-guard'
```

Start the enforcer in a dedicated terminal and keep it in the foreground.

```bash
sudo prlimit --memlock=unlimited:unlimited -- \
  env \
  ACTPLANE_PINNED_PROFILE=agent-file-guard \
  ACTPLANE_BPF_PIN_ROOT="$AGENTSIGHT_E2E_PIN_ROOT" \
  AGENTSIGHT_ENFORCER_SOCKET="$AGENTSIGHT_E2E_SOCKET" \
  RUST_LOG=info \
  "$AGENTSIGHT_ROOT/target/release/agentsight-enforcer"
```

Stop the test and retain logs if verifier, map, pinning, memlock, profile, or singleton-runtime errors occur.

Start the AgentSight API server in a second dedicated terminal.

```bash
sudo env \
  AGENTSIGHT_ENFORCER_SOCKET="$AGENTSIGHT_E2E_SOCKET" \
  RUST_LOG=info \
  "$AGENTSIGHT_ROOT/target/release/agentsight" serve \
    --host 127.0.0.1 \
    --port "$AGENTSIGHT_E2E_PORT" \
    --db "$AGENTSIGHT_E2E_STATE/genai_events.db" \
    --config "$AGENTSIGHT_ROOT/agentsight.json"
```

Read the dashboard token and fail closed unless the API reports the real backend and deletion capability.

```bash
export AGENTSIGHT_E2E_TOKEN=$(sudo sed -n '1p' \
  "$AGENTSIGHT_E2E_STATE/.dashboard_token")
test -n "$AGENTSIGHT_E2E_TOKEN"

AGENTSIGHT_E2E_HEALTH=$(curl --fail-with-body -sS \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/health")
printf '%s\n' "$AGENTSIGHT_E2E_HEALTH" | jq .
printf '%s\n' "$AGENTSIGHT_E2E_HEALTH" | jq -e '
  .ready == true and
  .backend == "actplane" and
  .capabilities.test_development == false and
  .capabilities.file_delete_guard == true
'

curl --fail-with-body -sS \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/bindings" | \
  jq -e '.bindings | length == 0'
```

## Start OpenClaw and identify the gateway

Run the gateway as the same non-root user that will own the canary file. Configure it with a non-dev, non-ephemeral local profile. Use `openclaw onboard` or `openclaw config` to set `gateway.mode=local`, `gateway.auth` to `token` (or `password`), and a valid model provider. Do not start with `--dev`, `--allow-unconfigured`, or `--auth none` in production-like instructions; those modes are only for temporary local debugging and the gateway CLI will refuse unattended websocket agent connections without real auth.

```bash
# example: generate a gateway token and persist it
export OPENCLAW_GATEWAY_TOKEN=$(openssl rand -hex 32)
openclaw config set gateway.mode local
openclaw config set gateway.auth token
openclaw config set gateway.auth.token --ref-provider default --ref-source env --ref-id OPENCLAW_GATEWAY_TOKEN
# The minimal tool profile no longer implicitly widens when tool sections are configured;
# explicitly allow the tools the agent needs for this test.
openclaw config set tools.alsoAllow --json '["exec", "process", "read", "write", "edit"]'
# configure your model provider separately; do not embed API keys in committed files
openclaw config validate
```

Start the local gateway in a third terminal as `$OPENCLAW_USER`. Do not use container mode, force resets, or remote execution.

```bash
su - "$OPENCLAW_USER" -s /bin/bash -c 'OPENCLAW_GATEWAY_TOKEN='"$OPENCLAW_GATEWAY_TOKEN"'; export OPENCLAW_GATEWAY_TOKEN; openclaw gateway run'
```

In a fourth terminal, start the TUI and identify the long-running gateway PID rather than the TUI, shell wrapper, or query command.

```bash
openclaw tui

pgrep -af 'openclaw-gatewa|node.*openclaw.*gatewa'
export OPENCLAW_GATEWAY_PID=<gateway-pid>

ps -o pid,ppid,lstart,cmd -p "$OPENCLAW_GATEWAY_PID"
tr '\0' ' ' <"/proc/$OPENCLAW_GATEWAY_PID/cmdline"
printf '\n'

export OPENCLAW_GATEWAY_START=$(sed -E 's/^.*\) //' \
  "/proc/$OPENCLAW_GATEWAY_PID/stat" | awk '{print $20}')
case "$OPENCLAW_GATEWAY_START" in
  ''|*[!0-9]*) echo 'invalid gateway start time' >&2; exit 1 ;;
esac
```

Restart this identification step if the gateway restarts.

## Create the canary file

Create a file that the gateway user would normally be able to remove. The canary must be owned by `$OPENCLAW_USER`; otherwise an `EPERM` could be ordinary permissions rather than policy enforcement.

```bash
printf 'OpenClaw delete guard canary\n' >"$AGENTSIGHT_E2E_PROTECTED"
chown "$OPENCLAW_USER:$OPENCLAW_USER" "$AGENTSIGHT_E2E_PROTECTED"
chmod 0600 "$AGENTSIGHT_E2E_PROTECTED"
stat "$AGENTSIGHT_E2E_PROTECTED"
test "$(LC_ALL=C printf '%s' "$AGENTSIGHT_E2E_PROTECTED" | wc -c)" -lt 64
```

## Start AgentSecCore and create the binding

Start the V2 daemon in a separate terminal. The V2 daemon has no CLI flags for AgentSight URL or token path; it hardcodes `http://127.0.0.1:7396/api` and `/var/log/sysak/.agentsight/.dashboard_token`. Copy the dashboard token to that exact path before starting the daemon.

```bash
printf '%s\n' "$AGENTSIGHT_E2E_TOKEN" > "$ASC_E2E_DIR/agentsight.token"
chmod 600 "$ASC_E2E_DIR/agentsight.token"
sudo install -d -m 0755 /var/log/sysak/.agentsight
printf '%s\n' "$AGENTSIGHT_E2E_TOKEN" | sudo tee /var/log/sysak/.agentsight/.dashboard_token >/dev/null
sudo chmod 600 /var/log/sysak/.agentsight/.dashboard_token

cd "$ASC_V2_DIR"
cargo build -p asc-daemon -p asc-cli
rm -f "$ASC_DAEMON_SOCKET"

"$ASC_V2_DIR/target/debug/agent-sec-daemon" serve \
  --socket "$ASC_DAEMON_SOCKET"
```

In the control terminal, create a dedicated policy and PID scope.

```bash
test -S "$ASC_DAEMON_SOCKET"
export ASC_POLICY_FILE="$ASC_E2E_DIR/openclaw-file-delete-guard.json"

cat > "$ASC_POLICY_FILE" <<JSON
{
  "kind": "prevent_file_deletion",
  "files": ["$AGENTSIGHT_E2E_PROTECTED"]
}
JSON

POLICY_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  policy create \
  --name openclaw-file-delete-guard \
  --file "$ASC_POLICY_FILE")
printf '%s\n' "$POLICY_JSON" | jq .
export POLICY_ID=$(printf '%s\n' "$POLICY_JSON" | jq -r '.policyId')
export POLICY_REVISION=$(printf '%s\n' "$POLICY_JSON" | jq -r '.revision')

SCOPE_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  scope create \
  --pid "$OPENCLAW_GATEWAY_PID")
printf '%s\n' "$SCOPE_JSON" | jq .
export SCOPE_ID=$(printf '%s\n' "$SCOPE_JSON" | jq -r '.scopeId')
export SCOPE_REVISION=$(printf '%s\n' "$SCOPE_JSON" | jq -r '.revision')
```

Create the binding without `--binding-id`. Let AgentSecCore allocate the ID and capture it from the response.

```bash
BINDING_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  binding create \
  --policy-id "$POLICY_ID" \
  --policy-revision "$POLICY_REVISION" \
  --scope-id "$SCOPE_ID" \
  --scope-revision "$SCOPE_REVISION")
printf '%s\n' "$BINDING_JSON" | jq .

export AGENTSECCORE_BINDING_ID=$(printf '%s\n' "$BINDING_JSON" | jq -r '.spec.bindingId')
export AGENTSECCORE_BINDING_REVISION=$(printf '%s\n' "$BINDING_JSON" | jq -r '.spec.bindingRevision')
case "$AGENTSECCORE_BINDING_ID:$AGENTSECCORE_BINDING_REVISION" in
  :*|*:null|*:*[^0-9]*) echo 'binding response lacks an ID or revision' >&2; exit 1 ;;
esac
```

Derive the AgentSight target binding ID from the allocated SecCore ID and revision. Use the derived ID only for AgentSight status, receipts, and cleanup.

```bash
export AGENTSIGHT_E2E_TARGET_UUID_NAME="urn:agentseccore:agentsight-binding:${AGENTSECCORE_BINDING_ID}:revision:${AGENTSECCORE_BINDING_REVISION}"
export AGENTSIGHT_E2E_BINDING_ID=$(python3 - <<'PY'
import os
import uuid

print(uuid.uuid5(uuid.NAMESPACE_URL, os.environ['AGENTSIGHT_E2E_TARGET_UUID_NAME']))
PY
)
printf 'seccore_binding_id=%s revision=%s\n' \
  "$AGENTSECCORE_BINDING_ID" "$AGENTSECCORE_BINDING_REVISION"
printf 'agentsight_binding_id=%s\n' "$AGENTSIGHT_E2E_BINDING_ID"
```

If the binding reaches `APPLY_FAILED` because AgentSight reports `max_active_bindings` exceeded, delete any previously enforced binding for this target UUID from AgentSight and retry with `binding update` on the same SecCore binding ID. SecCore allocates the binding ID automatically; do not pass `--binding-id` to `binding create`.

```bash
# example retry after deleting the stale AgentSight binding
"$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  binding update \
  --binding-id "$AGENTSECCORE_BINDING_ID"
```

Poll the asynchronous apply operation until it reaches `READY`, then verify AgentSight has the expected active binding.

```bash
for attempt in $(seq 1 50); do
  STATUS_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
    --socket "$ASC_DAEMON_SOCKET" \
    binding get \
    --binding-id "$AGENTSECCORE_BINDING_ID")
  STATUS=$(printf '%s\n' "$STATUS_JSON" | jq -r '.status')
  printf 'attempt=%s status=%s\n' "$attempt" "$STATUS"
  [ "$STATUS" != PENDING_APPLY ] && [ "$STATUS" != APPLYING ] && break
  sleep 0.1
done

test "$STATUS" = READY

AGENTSIGHT_E2E_BINDING_RESPONSE=$(curl --fail-with-body -sS \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/bindings" | \
  jq -c --arg id "$AGENTSIGHT_E2E_BINDING_ID" \
  '(.bindings // []) | map(select((.request.binding_id // .binding_id) == $id)) | first // empty')
test -n "$AGENTSIGHT_E2E_BINDING_RESPONSE"
printf '%s\n' "$AGENTSIGHT_E2E_BINDING_RESPONSE" | jq -e \
  --arg id "$AGENTSIGHT_E2E_BINDING_ID" '
    (.request.binding_id // .binding_id) == $id and
    .state == "enforced" and
    .domain_id != null
  '
```

Do not continue if application fails or AgentSight does not report the derived binding as enforced.

## Trigger and verify the deletion attempt

Ask the OpenClaw TUI to execute the following exact local shell command and return the raw stderr and exit code.

```text
sh -c 'echo $$ > /tmp/oc-delete-guard-tool.pid; sleep 20; exec rm -f /tmp/oc-delete-guard'
```

During the wait window, prove that the tool process is a descendant of the gateway.

```bash
for attempt in $(seq 1 100); do
  test -s /tmp/oc-delete-guard-tool.pid && break
  sleep 0.1
done

test -s /tmp/oc-delete-guard-tool.pid
export OPENCLAW_TOOL_PID=$(sed -n '1p' /tmp/oc-delete-guard-tool.pid)
case "$OPENCLAW_TOOL_PID" in
  ''|*[!0-9]*) echo 'invalid OpenClaw tool PID' >&2; exit 1 ;;
esac

export OPENCLAW_TOOL_START=$(sed -E 's/^.*\) //' \
  "/proc/$OPENCLAW_TOOL_PID/stat" | awk '{print $20}')
case "$OPENCLAW_TOOL_START" in
  ''|*[!0-9]*) echo 'invalid OpenClaw tool start time' >&2; exit 1 ;;
esac

ancestor=$OPENCLAW_TOOL_PID
is_descendant=0
while test "$ancestor" -gt 1; do
  if test "$ancestor" -eq "$OPENCLAW_GATEWAY_PID"; then
    is_descendant=1
    break
  fi
  test -r "/proc/$ancestor/stat" || break
  ancestor=$(sed -E 's/^.*\) //' "/proc/$ancestor/stat" | awk '{print $2}')
done
test "$is_descendant" -eq 1
```

Expect `rm` to fail with `Operation not permitted`, then independently confirm that the canary remains.

```bash
if test -e "$AGENTSIGHT_E2E_PROTECTED"; then
  echo 'PASS: protected file survived the OpenClaw delete attempt'
else
  echo 'FAIL: OpenClaw deleted the protected file' >&2
  exit 1
fi
```

Poll AgentSight for the receipt. Treat a normalized `operation` value of `write` as valid when all binding and blocking fields match.

```bash
AGENTSIGHT_E2E_VIOLATION=''
for attempt in $(seq 1 20); do
  AGENTSIGHT_E2E_VIOLATIONS=$(curl --fail-with-body -sS \
    -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
    "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/violations?limit=100")
  AGENTSIGHT_E2E_VIOLATION=$(printf '%s\n' "$AGENTSIGHT_E2E_VIOLATIONS" | \
    jq -c --arg id "$AGENTSIGHT_E2E_BINDING_ID" \
    '[.violations[] | select(.binding_id == $id)] | first // empty')
  test -n "$AGENTSIGHT_E2E_VIOLATION" && break
  sleep 0.5
done

test -n "$AGENTSIGHT_E2E_VIOLATION"
printf '%s\n' "$AGENTSIGHT_E2E_VIOLATION" | jq -e \
  --arg id "$AGENTSIGHT_E2E_BINDING_ID" \
  --argjson pid "$OPENCLAW_TOOL_PID" \
  --argjson start "$OPENCLAW_TOOL_START" '
    .binding_id == $id and
    .pid == $pid and
    .process_start_time == $start and
    .effect == "block" and
    .blocked == true and
    .killed == false
  '
```

## Verify scope and clean up

Before detaching, prove that the same gateway user outside the gateway process tree can still delete the canary. This confirms the block came from the scoped policy, not from ordinary file permissions.

```bash
# Recreate the canary first if the blocked OpenClaw attempt already removed it
if test ! -e "$AGENTSIGHT_E2E_PROTECTED"; then
  printf 'OpenClaw delete guard canary\n' >"$AGENTSIGHT_E2E_PROTECTED"
  chown "$OPENCLAW_USER:$OPENCLAW_USER" "$AGENTSIGHT_E2E_PROTECTED"
  chmod 0600 "$AGENTSIGHT_E2E_PROTECTED"
fi

su - "$OPENCLAW_USER" -s /bin/bash -c 'PATH_TO_DELETE='"$AGENTSIGHT_E2E_PROTECTED"'; rm -f -- "$PATH_TO_DELETE"'
test ! -e "$AGENTSIGHT_E2E_PROTECTED"
```

Detach the AgentSight binding, stop the test processes in this order, and only then remove the exact test resources.

```bash
status=$(curl --fail-with-body -sS -o /dev/null -w '%{http_code}' \
  -X DELETE \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/bindings/$AGENTSIGHT_E2E_BINDING_ID")
test "$status" = 204

# Stop the OpenClaw TUI, OpenClaw gateway, AgentSight server, AgentSight enforcer, and agent-sec-daemon.
# In a non-interactive shell, send SIGTERM to the PIDs identified earlier instead of Ctrl-C.
pgrep -af 'agentsight-enforcer|agentsight serve|openclaw-gatewa|agent-sec-daemon' || true

test "$AGENTSIGHT_E2E_PIN_ROOT" = /sys/fs/bpf/actplane-openclaw-e2e/v1 || exit 1
sudo rm -rf -- /sys/fs/bpf/actplane-openclaw-e2e
sudo rm -rf -- /run/agentsight-openclaw-e2e
sudo rm -rf -- /tmp/agentsight-openclaw-e2e
rm -rf -- "$ASC_E2E_DIR"
rm -f -- /tmp/oc-delete-guard /tmp/oc-delete-guard-tool.pid
```

Do not remove `/tmp/actplane.runtime.lock` or unmount shared `securityfs` or `bpffs`.

## Directory protection and policy update flow

This scenario verifies that a directory path is protected only when the policy uses a glob pattern (`/tmp/test/**`), not when it names the directory itself (`/tmp/test`). It also exercises the `policy update` and `binding update` commands.

Set an additional test directory variable and create sample files.

```bash
export AGENTSIGHT_E2E_PROTECTED_DIR=/tmp/test

sudo rm -rf -- "$AGENTSIGHT_E2E_PROTECTED_DIR"
install -d -m 0755 "$AGENTSIGHT_E2E_PROTECTED_DIR"
install -d -m 0755 "$AGENTSIGHT_E2E_PROTECTED_DIR/aaa"
printf 'file1\n' >"$AGENTSIGHT_E2E_PROTECTED_DIR/file1.txt"
printf 'nested\n' >"$AGENTSIGHT_E2E_PROTECTED_DIR/aaa/bbb.txt"
chown -R "$OPENCLAW_USER:$OPENCLAW_USER" "$AGENTSIGHT_E2E_PROTECTED_DIR"
```

Create a policy that names the directory exactly. The compiled policy uses an `exact` matcher, so it protects only the directory inode itself.

```bash
cat > "$ASC_E2E_DIR/dir-policy.json" <<JSON
{
  "kind": "prevent_file_deletion",
  "files": ["$AGENTSIGHT_E2E_PROTECTED_DIR"]
}
JSON

DIR_POLICY_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  policy create \
  --name dir-delete-guard \
  --file "$ASC_E2E_DIR/dir-policy.json")
printf '%s\n' "$DIR_POLICY_JSON" | jq .
export DIR_POLICY_ID=$(printf '%s\n' "$DIR_POLICY_JSON" | jq -r '.policyId')
export DIR_POLICY_REVISION=$(printf '%s\n' "$DIR_POLICY_JSON" | jq -r '.revision')
```

Create a binding against the same scope used for the single-file test, or create a new scope if the gateway PID has changed. Poll until it reaches `READY`.

```bash
DIR_BINDING_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  binding create \
  --policy-id "$DIR_POLICY_ID" \
  --policy-revision "$DIR_POLICY_REVISION" \
  --scope-id "$SCOPE_ID" \
  --scope-revision "$SCOPE_REVISION")
printf '%s\n' "$DIR_BINDING_JSON" | jq .
export DIR_BINDING_ID=$(printf '%s\n' "$DIR_BINDING_JSON" | jq -r '.spec.bindingId')
export DIR_BINDING_REVISION=$(printf '%s\n' "$DIR_BINDING_JSON" | jq -r '.spec.bindingRevision')

for attempt in $(seq 1 50); do
  DIR_STATUS_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
    --socket "$ASC_DAEMON_SOCKET" \
    binding get \
    --binding-id "$DIR_BINDING_ID")
  DIR_STATUS=$(printf '%s\n' "$DIR_STATUS_JSON" | jq -r '.status')
  printf 'attempt=%s status=%s\n' "$attempt" "$DIR_STATUS"
  [ "$DIR_STATUS" != PENDING_APPLY ] && [ "$DIR_STATUS" != APPLYING ] && break
  sleep 0.1
done
test "$DIR_STATUS" = READY
```

Ask the OpenClaw TUI to delete a file inside the directory. With the exact `/tmp/test` matcher, the deletion should succeed because the directory contents are not yet protected.

```bash
# run the OpenClaw delete attempt for /tmp/test/file1.txt
```

After confirming the file was deleted (or that the attempt succeeded), update the policy to use a glob that covers the directory tree.

```bash
printf 'file1\n' >"$AGENTSIGHT_E2E_PROTECTED_DIR/file1.txt"
chown "$OPENCLAW_USER:$OPENCLAW_USER" "$AGENTSIGHT_E2E_PROTECTED_DIR/file1.txt"

cat > "$ASC_E2E_DIR/dir-policy.json" <<JSON
{
  "kind": "prevent_file_deletion",
  "files": ["$AGENTSIGHT_E2E_PROTECTED_DIR/**"]
}
JSON

DIR_POLICY_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  policy update \
  --policy-id "$DIR_POLICY_ID" \
  --name dir-delete-guard \
  --file "$ASC_E2E_DIR/dir-policy.json")
printf '%s\n' "$DIR_POLICY_JSON" | jq .
export DIR_POLICY_REVISION=$(printf '%s\n' "$DIR_POLICY_JSON" | jq -r '.revision')
case "$DIR_POLICY_REVISION" in
  ''|*[!0-9]*) echo 'policy update did not return a new revision' >&2; exit 1 ;;
esac
```

Update the binding to reference the new policy revision. The scope revision is unchanged unless the gateway restarted.

```bash
DIR_BINDING_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
  --socket "$ASC_DAEMON_SOCKET" \
  binding update \
  --binding-id "$DIR_BINDING_ID" \
  --policy-id "$DIR_POLICY_ID" \
  --policy-revision "$DIR_POLICY_REVISION" \
  --scope-id "$SCOPE_ID" \
  --scope-revision "$SCOPE_REVISION")
printf '%s\n' "$DIR_BINDING_JSON" | jq .
export DIR_BINDING_REVISION=$(printf '%s\n' "$DIR_BINDING_JSON" | jq -r '.spec.bindingRevision')

for attempt in $(seq 1 50); do
  DIR_STATUS_JSON=$("$ASC_V2_DIR/target/debug/agent-sec-cli" \
    --socket "$ASC_DAEMON_SOCKET" \
    binding get \
    --binding-id "$DIR_BINDING_ID")
  DIR_STATUS=$(printf '%s\n' "$DIR_STATUS_JSON" | jq -r '.status')
  printf 'attempt=%s status=%s\n' "$attempt" "$DIR_STATUS"
  [ "$DIR_STATUS" != PENDING_APPLY ] && [ "$DIR_STATUS" != APPLYING ] && break
  sleep 0.1
done
test "$DIR_STATUS" = READY
```

Derive the new AgentSight binding UUID from the updated SecCore binding revision and confirm it is `enforced`. The AgentSight binding ID changes when the SecCore binding revision changes.

```bash
export AGENTSIGHT_DIR_TARGET_UUID_NAME="urn:agentseccore:agentsight-binding:${DIR_BINDING_ID}:revision:${DIR_BINDING_REVISION}"
export AGENTSIGHT_DIR_BINDING_ID=$(python3 - <<'PY'
import os
import uuid

print(uuid.uuid5(uuid.NAMESPACE_URL, os.environ['AGENTSIGHT_DIR_TARGET_UUID_NAME']))
PY
)

AGENTSIGHT_DIR_BINDING_RESPONSE=$(curl --fail-with-body -sS \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/bindings" | \
  python3 - "$AGENTSIGHT_DIR_BINDING_ID" <<'PY'
import json, sys
binding_id = sys.argv[1]
data = json.loads(sys.stdin.read())
for b in data.get('bindings', []):
    bid = b.get('request', {}).get('binding_id') or b.get('binding_id')
    if bid == binding_id:
        print(json.dumps(b))
        break
PY
)
test -n "$AGENTSIGHT_DIR_BINDING_RESPONSE"
printf '%s\n' "$AGENTSIGHT_DIR_BINDING_RESPONSE" | jq -e '
    .state == "enforced" and
    .domain_id != null
  '
```

Trigger OpenClaw deletion attempts against both a top-level file and a nested file. Both should fail with `Operation not permitted`.

```bash
# run the OpenClaw delete attempt for /tmp/test/file1.txt
# run the OpenClaw delete attempt for /tmp/test/aaa/bbb.txt
```

Confirm the files survived and the same gateway user can still delete them from a shell outside the gateway process tree.

```bash
if test -e "$AGENTSIGHT_E2E_PROTECTED_DIR/file1.txt" && \
   test -e "$AGENTSIGHT_E2E_PROTECTED_DIR/aaa/bbb.txt"; then
  echo 'PASS: glob policy blocked files inside the directory'
else
  echo 'FAIL: protected files were deleted' >&2
  exit 1
fi

su - "$OPENCLAW_USER" -s /bin/bash -c 'PATH_TO_DELETE='"$AGENTSIGHT_E2E_PROTECTED_DIR"'; rm -f -- "$PATH_TO_DELETE/file1.txt" "$PATH_TO_DELETE/aaa/bbb.txt"'
test ! -e "$AGENTSIGHT_E2E_PROTECTED_DIR/file1.txt"
test ! -e "$AGENTSIGHT_E2E_PROTECTED_DIR/aaa/bbb.txt"
```

Poll AgentSight for violations tied to the updated binding. The violations should list both blocked targets with `effect=block`, `blocked=true`, and `killed=false`.

```bash
for attempt in $(seq 1 20); do
  AGENTSIGHT_DIR_VIOLATIONS=$(curl --fail-with-body -sS \
    -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
    "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/violations?limit=100")
  AGENTSIGHT_DIR_VIOLATION_COUNT=$(printf '%s\n' "$AGENTSIGHT_DIR_VIOLATIONS" | \
    python3 - "$AGENTSIGHT_DIR_BINDING_ID" <<'PY'
import json, sys
binding_id = sys.argv[1]
data = json.loads(sys.stdin.read())
print(len([v for v in data.get('violations', []) if v.get('binding_id') == binding_id]))
PY
)
  printf 'attempt=%s violation_count=%s\n' "$attempt" "$AGENTSIGHT_DIR_VIOLATION_COUNT"
  test "$AGENTSIGHT_DIR_VIOLATION_COUNT" -ge 2 && break
  sleep 0.5
done
test "$AGENTSIGHT_DIR_VIOLATION_COUNT" -ge 2
```

Detach the directory binding before cleaning up.

```bash
status=$(curl --fail-with-body -sS -o /dev/null -w '%{http_code}' \
  -X DELETE \
  -H "Authorization: Bearer $AGENTSIGHT_E2E_TOKEN" \
  "$AGENTSIGHT_E2E_BASE_URL/api/enforcement/bindings/$AGENTSIGHT_DIR_BINDING_ID")
test "$status" = 204
```

## Troubleshooting

This section records problems that have caused real test failures so future runs can avoid them.

### AgentSecCore V2 hardcodes AgentSight URL and token path

`agent-sec-daemon serve` accepts no `--agentsight-url` or `--agentsight-token-file` flags. It always talks to `http://127.0.0.1:7396/api` and reads `/var/log/sysak/.agentsight/.dashboard_token`. Run AgentSight on port `7396` and copy the token to that exact path before starting the daemon.

### Binding ID is allocated by AgentSecCore

Do not pass `--binding-id` to `agent-sec-cli binding create`. Capture `spec.bindingId` and `spec.bindingRevision` from the response and derive the AgentSight binding UUID from them.

### `max_active_bindings=1`

AgentSight currently allows only one active binding. If `binding get` reports `APPLY_FAILED` and the enforcer logs mention `max_active_bindings`, delete the stale AgentSight binding (using the derived UUID from a previous run) and call `binding update --binding-id "$AGENTSECCORE_BINDING_ID"` to retry. Do not create a second SecCore binding.

### OpenClaw gateway must use real auth

`--dev`, `--allow-unconfigured`, and `--auth none` are only for temporary local debugging. The test requires `gateway.mode=local` with `gateway.auth=token` (or `password`) and a valid gateway token. Start the gateway as `$OPENCLAW_USER`, not root.

### LLM context length and model validation

If the TUI agent fails to emit the `exec` tool call, returns a malformed call, or the provider rejects the prompt with a context-length error, switch the TUI to Code Mode and reduce the exposed tool surface. Set `tools.profile=minimal` and `tools.alsoAllow = ["exec", "process", "read", "write", "edit"]` in `openclaw.json` (or via `openclaw config set tools.profile minimal` and `openclaw config set tools.alsoAllow --json '[...]'`). Verify the configured model name exists at the provider endpoint. For example, some Alibaba endpoints expose `qwen3.7-max` but not `qwen3.6-max`.

### Canary ownership

The canary must be owned by `$OPENCLAW_USER`. If root owns the file, the gateway will receive `EPERM` from ordinary permissions and the test proves nothing about policy enforcement.

## Pass criteria

Record PASS only when all of the following are true:

- Enforcer starts without verifier, map, pinning, memlock, or profile errors.
- Health reports `ready=true`, `backend=actplane`, `test_development=false`, and `file_delete_guard=true`.
- The dynamically allocated binding reaches `READY`; its derived AgentSight binding is enforced and has a domain ID.
- A gateway descendant receives `EPERM` while the canary remains.
- A matching violation reports `effect=block`, `blocked=true`, and `killed=false`.
- The gateway user can delete the canary from a shell outside the gateway process tree before detach.

Save the kernel release, repository revision, health JSON, gateway and tool identities, binding IDs and state, command stderr and exit code, violation JSON, cleanup result, and relevant logs with every test result.
