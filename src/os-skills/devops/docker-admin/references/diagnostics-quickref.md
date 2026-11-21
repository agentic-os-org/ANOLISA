# Docker diagnostics quick reference (single host)

Read-only commands, ordered global → object → detail. Safe to run on
production hosts at any time.

## Global state

| Command | Purpose |
|---------|---------|
| `docker info` | Daemon version, storage driver, data-root, permissions |
| `docker system df` | Totals and reclaimable space per object type |
| `docker system df -v \| head -80` | Per-image / per-container / per-volume detail (bound it - the longest listing) |
| `docker ps` | Running containers |
| `docker ps -a --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}'` | All containers with exit status |

## Single container

| Command | Purpose |
|---------|---------|
| `docker inspect <c>` | Source of truth for config and state (networks, mounts, restart policy, exit code) |
| `docker inspect --format '{{.State.Status}} {{.State.ExitCode}} {{.State.OOMKilled}}' <c>` | Three key facts on one line |
| `docker inspect --format '{{json .State.Health}}' <c> \| jq` | Health check probe history |
| `docker stats --no-stream <c>` | Single-container resource snapshot |
| `docker top <c>` | Processes inside the container |
| `docker port <c>` | Port mappings |
| `docker logs --tail 100 --since 30m <c>` | Bounded log window (always bound log output) |
| `docker exec <c> sh -c 'command'` | Run one observation command without a tty |

## Disk and log footprint

| Command | Purpose |
|---------|---------|
| `df -h "$(sudo docker info --format '{{.DockerRootDir}}')"` | Free space on the filesystem holding the docker root (data-root may be customized) |
| `DATA_ROOT=$(sudo docker info --format '{{.DockerRootDir}}'); sudo sh -c "du -sh '$DATA_ROOT'/containers/*/*-json.log \| sort -h \| tail"` | Largest container log files (the glob must expand inside the privileged shell, and data-root must be read with sudo too) |
| `DATA_ROOT=$(sudo docker info --format '{{.DockerRootDir}}'); sudo du -sh "$DATA_ROOT"/overlay2` | Total image layer footprint |
| `docker volume ls` / `docker volume inspect <v>` | Volume inventory and mountpoints |

## Compose project

| Command | Purpose |
|---------|---------|
| `docker compose config` | Validate compose file (run after every edit) |
| `docker compose ps` | Project container status |
| `docker compose top` | Project processes |
| `docker compose logs --tail 50 <svc>` | Single-service logs |

## Daemon side

| Command | Purpose |
|---------|---------|
| `sudo journalctl -u docker -n 100 --no-pager` | Daemon startup failure / crash logs |
| `sudo systemctl status docker` | Service status |
