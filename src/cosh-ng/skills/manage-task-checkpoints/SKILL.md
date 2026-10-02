---
name: manage-task-checkpoints
description: >
  Inspect and recover snapshots owned by a COSH managed Task. Use when the
  user asks the outer COSH Agent to list Task snapshots, preview or diff an
  exact recovery point, switch a Task workspace, or recover a workspace-only
  failure and retry once. Do not use for standalone ws-ckpt workspaces.
---

# Manage COSH Task Checkpoints

Treat this Skill as a recovery workflow, not a security boundary. Gateway and
ws-ckpt enforce Task ownership, complete snapshot IDs, workspace generation,
live diff, peer identity, and replay safety.

## Keep the scope Task-only

- Use this Skill only for `/task`, `tsk_...`, or an explicitly managed Task.
- Use the separate `ws-ckpt` Skill for a standalone workspace.
- Do not infer the managed workspace from cwd or ask the user for its path.
- Do not call bare `ws-ckpt`, a lab script, or the ws-ckpt socket for a Task.

## Inspect Task snapshots

Use the Task-scoped Gateway CLI. It resolves an explicit socket, then
`COSH_GATEWAY_SOCKET`, then the active packaged Gateway instance:

```bash
cosh-gateway task --output jsonl list
cosh-gateway task --output jsonl snapshot list <task-id>
cosh-gateway task --output jsonl snapshot preview <task-id> <snapshot-id>
cosh-gateway task --output jsonl snapshot diff <task-id> <snapshot-id>
```

Accept only complete Task and snapshot IDs returned by Gateway. Never guess or
expand a prefix. When several Tasks or recovery points match, summarize their
status and file effects and ask the user to choose by meaning rather than
copying internal IDs.

Users can inspect the same state directly with:

```text
/task snapshots <task-id>
/task snapshot preview <task-id> <snapshot-id>
/task snapshot diff <task-id> <snapshot-id>
```

## Switch safely

1. Require the Task to be terminal before switching.
2. Ensure the outer `cosh-shell` process was launched outside the managed
   workspace. Running `cd /tmp` inside an Agent Bash command changes only the
   child process and does not release the parent shell cwd.
3. Preview the exact snapshot immediately before switching.
4. Describe the files that will be removed, restored, or retained. Ask for
   natural-language confirmation unless the user already authorized this one
   exact recovery attempt.
5. Submit the exact snapshot ID, preview digest, terminal Task revision, and a
   fresh idempotency key for this operation.
6. Verify the resulting file state against the preview.

```bash
cosh-gateway task --output jsonl snapshot switch <task-id> <snapshot-id> \
  --preview-digest <digest> \
  --expected-revision <revision> \
  --idempotency-key <fresh-key>
```

Do not display Task IDs, snapshot IDs, revisions, digests, approval IDs, or
idempotency keys in ordinary progress or final prose. Keep them in the actual
command or approval card. Describe recovery points by step and affected files.

### Handle cwd and uncertain results

- On `CwdOccupied`, state that the target was not switched. Ask the user to
  exit the entire COSH session, start COSH from outside the workspace, and run
  a new preview. Do not retry the consumed operation key.
- On `unknown`, `possibly_applied`, or lost transport after dispatch, stop and
  report the uncertainty. Never generate another key to retry blindly.
- Never add `--force` or `--yes`, and never bypass Gateway with raw ws-ckpt.

## Recover and retry once

For an established workspace-file failure in a multi-step Task:

1. Stop further file writes.
2. Confirm that the failure has no network, cloud, service, credential,
   database, or other external side effect.
3. Prefer the nearest `pre_effect` before the bad step; use the baseline only
   when no suitable pre-effect checkpoint exists.
4. Preview and switch once using the workflow above.
5. Retry the failed step once with a corrected approach. Do not loop.
6. Report the human-readable recovery point, verification, retry result, and
   whether Gateway created a recovery snapshot that can undo the switch.

File snapshots do not restore external side effects. Stop automatic recovery
and identify the remaining state when any such effect may have occurred.
