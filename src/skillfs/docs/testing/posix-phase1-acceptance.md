# POSIX Phase 1 Acceptance Checklist

**Spec**: `docs/specs/posix-phase1-spec.md`
**Matrix**: `POSIX_FS_TEST_MATRIX.csv`

## Environment

Run full acceptance on Linux with:

- `/dev/fuse` available;
- `fusermount3` available;
- permission to mount FUSE filesystems;
- Rust toolchain compatible with this workspace.

When FUSE is unavailable, FUSE integration tests should skip gracefully.

## Required Commands

```bash
cargo check -p skillfs-fuse
cargo test -p skillfs-core
cargo test -p skillfs-fuse
cargo test -p skillfs-fuse --test write_guard_tests
cargo test -p skillfs-fuse --test posix_phase1_tests
```

## Functional Acceptance

### Existing SkillFS Behavior

- `/skills` or in-place root shows the configured primary view.
- `skill-discover/SKILL.md` remains readable and virtual.
- Reading `<skill>/SKILL.md` returns compiled content.
- Writing `<skill>/SKILL.md` updates the physical source file.
- Skill directory rename remains immediately visible under the new name.
- Stale frontmatter after rename does not resurrect the old skill name.

### POSIX P0 Behavior

- `open` honors read/write access modes.
- `open(O_APPEND)` appends data.
- `open(O_CREAT | O_EXCL)` fails when the target exists.
- `open(O_DIRECTORY)` fails for regular files.
- `open(O_TRUNC)` truncates writable files and reparses `SKILL.md`.
- offset reads and writes work without whole-file buffering.
- `flush` and `fsync` succeed for passthrough file handles.
- `statfs` reports non-zero source filesystem stats.
- `chmod` through the mount changes source file mode.
- timestamp update through the mount changes source file timestamps.
- `access` returns expected results for visible virtual paths and physical passthrough paths.
- `opendir`/`readdir` produce stable offsets for an opened directory stream.
- unsupported or invalid rename flags are not silently treated as plain rename.
- common errno values are preserved: `ENOENT`, `EEXIST`, `ENOTDIR`, `EISDIR`, `ENOTEMPTY`, `EACCES`/`EROFS`, `EINVAL`.

## Suggested Manual Smoke

Create a source with one skill:

```bash
mkdir -p /tmp/skillfs-src/demo-skill
cat >/tmp/skillfs-src/demo-skill/SKILL.md <<'EOF'
---
name: demo-skill
description: Demo skill
---

# Demo
EOF
mkdir -p /tmp/skillfs-mnt
cargo run -p skillfs -- mount /tmp/skillfs-src /tmp/skillfs-mnt --foreground
```

In another shell:

```bash
stat /tmp/skillfs-mnt/skills/demo-skill/SKILL.md
printf a >/tmp/skillfs-mnt/skills/demo-skill/data.txt
printf b >>/tmp/skillfs-mnt/skills/demo-skill/data.txt
cat /tmp/skillfs-src/demo-skill/data.txt
chmod 600 /tmp/skillfs-mnt/skills/demo-skill/data.txt
stat -c '%a' /tmp/skillfs-src/demo-skill/data.txt
sync
stat -f /tmp/skillfs-mnt/skills/demo-skill
```

Expected:

- source `data.txt` contains `ab`;
- source `data.txt` mode is `600`;
- `stat -f` reports non-zero block counts;
- mounted skill remains visible after these operations.

## Deferred For Later Phases

Do not block Phase 1 on:

- symlink creation/readlink;
- hard links;
- xattrs;
- device nodes/FIFOs through `mknod`;
- `copy_file_range`;
- `fallocate`;
- watcher-driven source updates;
- hot reload of `skillfs-views.toml`.
