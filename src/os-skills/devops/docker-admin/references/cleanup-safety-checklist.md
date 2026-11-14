# Cleanup safety checklist

Walk through this checklist before running any Docker/Podman destructive
command (`rm` / `prune` / `down --volumes`). If any item fails, stop and
return to read-only diagnostics.

## Before deleting (list first)

- [ ] A read-only inventory of the objects to be deleted has been produced
      (`docker ps -a`, `docker image ls`, `docker volume ls`,
      `docker system df -v | head -80`) and shown to the user; on hosts
      with many objects, bound the listing with `--format` plus
      `head` / `--filter`
- [ ] `docker system df` baseline recorded so the reclaimed space can be
      compared afterwards
- [ ] Stopped containers are not being actively investigated by a human
      (exit state can be incident evidence)
- [ ] Dangling images are build intermediates, not images referenced
      directly by `docker run <image-id>`
- [ ] Scope bounded with `--filter until=...` only where the subcommand
      supports it (container / image / network prune; builder prune uses
      `until=24h`) - volume prune has no time filter

## Volume protection (named volumes)

- [ ] No `docker volume prune [-a]` and no `docker system prune --volumes`
      will run (on API < v1.42 - Engine 19.03 / 20.10 - both also remove
      named volumes; treat them as extreme there)
- [ ] No `docker compose down --volumes` (unless the user confirmed each
      volume by name)
- [ ] Any single-volume removal uses the exact name: `docker volume rm <name>`,
      never wildcards
- [ ] Containers that mounted the volume were listed first
      (`docker ps -a --filter volume=<name>`)
- [ ] Database-class volumes (postgres / mysql / redis and similar) are not
      deleted by default, even if the user says "remove the old one"

## During execution

- [ ] One category at a time (containers, then images, then builder cache),
      re-checking `docker system df` between categories
- [ ] No single `docker system prune -a --volumes` wipe
- [ ] On any unexpected object (unknown container, unknown volume): stop
      immediately and return to the inventory

## After deletion

- [ ] `docker system df` compared against the baseline; actual reclaimed
      space reported to the user
- [ ] Business containers still healthy: `docker ps` shows no unexpected
      restarts
- [ ] If something was deleted by mistake: images can be re-pulled; volumes
      recover only from a pre-existing backup - which is why the backup
      question is asked before deleting

## Never (refuse outright, no checklist)

- `--privileged` containers as a troubleshooting tool
- Deleting the data-root directory itself to "free space"
- Any prune against a production database volume
