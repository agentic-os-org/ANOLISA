---
name: docker-admin
version: 0.1.0
description: Use this skill when managing a single-host Docker (or Podman) container runtime - checking container status and health, inspecting logs, troubleshooting crashed or restarting containers, reclaiming disk space with prune commands, or running docker compose lifecycle operations. Triggers on container diagnostics, docker ps/stats/df/logs/exec/inspect, image and volume cleanup, docker system prune, and docker compose up/down/restart/pull.
layer: application
lifecycle: production
category: devtools
tags: [docker, podman, container, compose, sysadmin, cleanup, troubleshooting]
dependencies: [shell-scripting]
platforms:
  - claude-code
  - gemini-cli
  - openai-codex
license: Apache-2.0
maintainers:
  - github: Zzzzzmeow
---

# Docker Container Administration

Single-host container runtime administration for Docker (with Podman
equivalents where they apply). This skill covers the day-to-day safety-critical
work of running containers on a server: diagnosing container state, reading
logs without flooding the terminal, troubleshooting restart loops and failed
health checks, reclaiming disk space without destroying data, and managing
docker compose project lifecycles. The host is treated as a production asset:
destructive operations are listed before they are run, named volumes are
protected by default, and every change can be answered with "why".

---

## When to use this skill

Trigger this skill when the user:

- Asks to check container status, resource usage, or a failing health check
- Wants to read or follow container logs, or exec into a container to look around
- Needs to troubleshoot a container that crashes, restart-loops, or exits immediately
- Wants to reclaim disk space: remove images, clean build cache, or run prune commands
- Asks to manage a multi-container app with `docker compose` (up / down / restart / pull)
- Reports a host problem caused by containers (disk filled by container logs, port conflicts)

Do NOT trigger this skill for:

- Kubernetes orchestration (pods, services, ingress, kubectl cluster management)
- Docker daemon configuration (`/etc/docker/daemon.json`, registry mirrors,
  data-root migration) - out of scope for this skill's first version
- Writing or optimizing Dockerfiles and image builds - an application development topic
- Containerized application code debugging - debug the app, not the runtime
- Product-bundled install or deployment flows (install-hermes, install-qwenpaw,
  qwenpaw-usage) - those skills own their own Dockerfile and compose usage

---

## Key principles

1. **List before deleting** - Every destructive operation (`rm`, `prune`,
   `down --volumes`) must be preceded by a read-only listing of exactly what
   will be removed, shown to the user for confirmation. `docker system prune`
   has no `--dry-run`; the listing step is yours to perform explicitly.
2. **Protect named volumes** - Named volumes hold persistent data (databases,
   uploads). Never run `docker volume prune` or `docker system prune --volumes`
   by default. Remove a named volume only by exact name, after the user
   confirms its data is disposable, and after showing which containers used it.
3. **Least privilege** - Access to `docker.sock` is equivalent to root. Do not
   change docker group membership or daemon configuration to work around a
   permission error. Use a one-off `sudo docker ...` for the task at hand.
   If the user wants long-term password-less access, present the command and
   its impact (root-equivalent, requires re-login) for them to decide, or
   propose rootless Docker / Podman instead. Never use `--privileged` as a
   troubleshooting shortcut - host-namespace options such as
   `--cap-add=SYS_PTRACE` with `--pid=host` are the same exposure class and
   need the same confirmation.
4. **Diagnose before acting** - Before changing or deleting anything, confirm
   the problem with read-only commands (`ps`, `inspect`, `df`, `logs`). Every
   write operation on a production host must answer "why".
5. **Idempotent lifecycles** - Prefer commands that are safe to re-run:
   `docker compose up -d` reconciles state, cleanup plans are reproducible,
   and compose files are backed up before editing.

---

## Step zero: probe the environment

Run these read-only checks before any other action:

```bash
# Runtime detection: which CLIs exist, then which daemon answers.
# A docker CLI with an unreachable daemon is NOT a podman host - never fall
# back silently. Run `podman version` only when `command -v` showed podman
# exists AND docker is confirmed unavailable (dead daemon or missing CLI).
command -v docker podman
docker version 2>&1 | head -20     # failure reason stays visible; no silent fallback
# Daemon liveness and whether the current user has permission
# (permission denied on docker.sock means the user is not in the docker group)
docker info 2>&1 | head -40
# Engine and API version - the prune semantics below assume
# Docker Engine 23.0+ / API >= v1.42
docker version --format '{{.Server.Version}} {{.Server.APIVersion}}'
# Storage driver and data-root location - required context for disk work
# (empty output here means no socket access - re-run with sudo before disk work)
docker info --format '{{.Driver}} {{.DockerRootDir}}'
# Compose availability (v2 is a docker subcommand, v1 a standalone binary)
docker compose version 2>/dev/null || docker-compose --version 2>/dev/null
```

On Podman most diagnostic commands map one-to-one (`podman ps`, `podman stats`,
`podman logs`, `podman inspect`); there is no daemon, no docker group, and
permissions go through user namespaces. Substitute equivalents rather than
assuming rootful mode. Compose on a pure Podman host additionally requires
`podman-compose` or a podman socket exported via `DOCKER_HOST` - verify this
before applying the compose section.

---

## Runtime diagnostics

```bash
docker ps                        # running containers
docker ps -a                     # include exited ones (exit code and when)
docker stats --no-stream         # per-container CPU / memory / net / IO snapshot
docker top <container>           # processes inside a container
docker inspect <container>       # full config and state - the source of truth
```

**Failed health check**: read the probe history with
`docker inspect --format '{{json .State.Health}}' <container>` - the `Log`
array holds recent probe output and exit codes. Common causes: the probe
command is missing from the image (for example no `curl`), the interval is too
short, or the process listens on `127.0.0.1` while the probe targets the
container IP from the network namespace.

**Restart loops**: check `.State.ExitCode` and `.State.RestartCount` with
`docker inspect`, then read the logs (next section) for the crash reason.
`ExitCode 137` means OOM kill or `docker kill` - confirm with
`.State.OOMKilled`.

## Logs and troubleshooting

```bash
docker logs --tail 100 <container>               # last 100 lines
docker logs --since 30m <container>              # last 30 minutes
docker logs <container> 2>&1 | grep -i error     # merge stderr, then filter
timeout 10 docker logs -f --tail 20 <container>  # bounded follow; never a bare -f (exit 124 = timeout cutoff, expected)
docker exec -i <container> sh -c 'cat /etc/os-release'   # one command, non-interactive
```

Safety rules:

- Container logs can be huge. **Always** bound output with `--tail` or
  `--since`; `--tail` bounds the starting lines, not the duration. For full
  analysis redirect to a temp file and process it there instead of streaming
  it to the conversation.
- Agent runtimes are non-interactive: never start a bare `docker logs -f`
  follow (it blocks until the runtime kills it, and buffered output may be
  lost) and never open an interactive `exec -it` shell session. Bound
  follows with `timeout` and run single commands via
  `docker exec -i <c> sh -c '<command>'`.
- `exec` runs as root inside the container (unless the image or compose sets
  `user`). Perform observation only (`cat`, `ls`, `ps`); do not hot-edit
  files inside a container - changes belong in the image or the compose file.
- Mind time zones: containers often run UTC while `--since` is interpreted
  on the host clock. Check `docker exec <container> date` first when
  correlating timestamps.

## Disk usage and cleanup

```bash
docker system df                 # totals and reclaimable per object type
docker system df -v | head -80   # per-object detail - the longest listing, keep it bounded
docker image ls --format '{{.Repository}}:{{.Tag}} {{.Size}}' | head -50   # bounded on busy hosts
docker volume ls --format '{{.Name}}' | grep -E '^[0-9a-f]{64}$'     # anonymous volumes only
docker volume ls --format '{{.Name}}' | grep -vE '^[0-9a-f]{64}$'    # named volumes only
```

**Destructive power of the prune family** (Docker Engine 23.0+ / API >= v1.42):

| Command | Removes | Risk |
|---------|---------|------|
| `docker container prune` | all stopped containers | low-medium (stopped containers may hold incident state) |
| `docker image prune` | dangling images only | low |
| `docker image prune -a` | all images not used by any container | medium (next start re-pulls) |
| `docker network prune` | unused networks | low |
| `docker builder prune` | build cache | low-medium (rebuilds get slower) |
| `docker volume prune` | unused **anonymous** volumes only (named volumes survive) | high (anonymous volumes can still hold data) |
| `docker volume prune -a` | **all unused volumes, named ones included** | **extreme (data deletion)** |
| `docker system prune` | stopped containers + dangling images + unused networks + build cache | medium |
| `docker system prune -a --volumes` | all of the above, plus unreferenced images and anonymous volumes | **extreme** |

On API versions below v1.42 (Engine 19.03 / 20.10 - still common on legacy
hosts), `docker volume prune` and `docker system prune --volumes` also
remove **named** volumes. Treat them at the same extreme level as
`docker volume prune -a` there.

Before any prune, take a read-only inventory and show the affected objects to
the user for confirmation. Bound the blast radius with `--filter until=...`
only where the subcommand supports it: container / image / network prune
accept `until=<timestamp>`, builder prune accepts `until=24h`; volume prune
supports only `all` / `label` filters, and `system prune`'s time window does
not apply to volumes. Do not invent filters that will be silently ignored.
Execute one category at a time (containers, then images, then builder cache)
and re-check `docker system df` in between.

Before deleting, walk through `references/cleanup-safety-checklist.md`.

## Compose lifecycle

The `compose.yaml` (or `docker-compose.yml`) in the project directory is the
single source of truth:

```bash
docker compose config           # validate file syntax and variable expansion (run after every edit)
docker compose up -d            # idempotent: creates/recreates only changed services
docker compose pull             # pull newer images (pair with up -d for rolling update)
docker compose ps               # project container status
docker compose logs --tail 50 <service>   # single-service logs
docker compose restart          # restart (does NOT re-read compose file changes)
docker compose down              # stop and remove containers + networks (keeps volumes)
docker compose down --volumes    # also removes named volumes - high risk, confirm each name
```

Key points:

- Config changes do NOT take effect via `restart` - they need `up -d`, which
  recreates the affected containers.
- `down` keeps volumes by design to protect data; `--volumes` deletes both
  named and anonymous volumes and is a data deletion operation that follows
  the named-volume protection rule.
- Back up the compose file (`cp compose.yaml compose.yaml.bak`) before
  editing, and validate with `docker compose config` before applying.

---

## Quick troubleshooting table

| Symptom | Locate with | Action |
|---------|-------------|--------|
| Container exits immediately | `docker inspect` `.State.ExitCode` + logs | Fix per exit code; 137 means OOM first |
| Restart loop | `.State.RestartCount` + logs | Find the crash cause in logs; check OOMKilled |
| Health check never passes | `{{json .State.Health}}` log array | Fix probe command / interval / listen address |
| Disk full under the docker data-root | `sudo docker info --format '{{.DockerRootDir}}'` + `sudo df -h <data-root>` | Follow the cleanup flow - one category at a time, never `prune -a --volumes` blindly |
| Container log fills the disk | `DATA_ROOT=$(sudo docker info --format '{{.DockerRootDir}}'); sudo sh -c "du -sh '$DATA_ROOT'/containers/*/*-json.log \| sort -h \| tail"` (the glob must expand inside the privileged shell) | With user confirmation: `sudo truncate -s 0 <file>` - never `rm` (it orphans the fd until the container restarts); log rotation config is a daemon topic (out of scope here) |
| `permission denied ... docker.sock` | `docker info` | Report the permission gap to the user with the exact command and its root-equivalent impact; do not auto-add the user to the docker group; use a one-off `sudo docker` only |
| Port already in use | `ss -ltnp \| grep <port>` | Find the occupying process; change the mapping or stop the old one |

More commands: `references/diagnostics-quickref.md`. Before any deletion:
`references/cleanup-safety-checklist.md`.

---

## High-risk confirmation list

Show the user the impact and obtain explicit confirmation before any of:

1. `docker volume rm` / `docker volume prune [-a]` / `compose down --volumes`
   (data deletion; on API < v1.42 the plain `volume prune` also removes named
   volumes)
2. `docker system prune -a` or any prune with `--volumes` (wide deletion)
3. Running a container with `--privileged` (host-level exposure)
4. `docker rm -f` on a running container not managed by compose (incident
   state may be lost)
5. Adding a user to the docker group or changing daemon authentication
   (persistent root-equivalent access; needs re-login to take effect)
6. Host-namespace container options such as `--pid=host` +
   `--cap-add=SYS_PTRACE` (same exposure class as `--privileged`)
7. Truncating or deleting container log files on disk (use
   `sudo truncate -s 0 <file>`, never `rm`)

For each confirmation provide: the list of affected objects, how to roll
back, and a less destructive alternative. Until the user agrees, run
read-only commands only.
