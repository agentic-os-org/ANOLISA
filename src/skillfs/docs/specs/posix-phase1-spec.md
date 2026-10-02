# SkillFS POSIX Phase 1 Specification

**Status**: baseline complete; retained as the Phase 1 behavior contract
**Target crate**: `skillfs-fuse`
**Capability record**: `docs/skillfs-filesystem-capability-record.md`

## 1. Scope

Phase 1 defines the minimum POSIX behavior required before SkillFS can be considered default-on-ready for Linux images.

The goal is not to make the whole filesystem fully POSIX-complete. The goal is to make common POSIX file operations correct for the physical namespace under skill directories while preserving SkillFS virtual semantics.

In scope:

- real file handle tracking for passthrough files;
- accurate common open/create flags;
- streaming offset read/write;
- `flush`, `fsync`, `fsyncdir`;
- `statfs`;
- `access`;
- metadata mutations through `setattr`;
- stable directory handles;
- rename flag validation;
- improved errno preservation;
- P0 tests from `POSIX_FS_TEST_MATRIX.csv`.

Out of scope:

- symlink creation and `readlink`;
- hard links;
- xattrs;
- special files;
- sparse-file advanced APIs;
- source watcher integration;
- view hot reload.

## 2. Namespace Model

SkillFS has two namespaces:

| Namespace | Examples | Phase 1 behavior |
|-----------|----------|------------------|
| virtual | `/`, `/skills`, `skill-discover`, compiled `SKILL.md` content | deterministic virtual attrs; write operations rejected unless explicitly allowed |
| physical passthrough | `<skill>/scripts/x.sh`, `<skill>/references/a.md`, source `SKILL.md` writes | backed by real source filesystem operations |

`SKILL.md` is mixed:

- Reads return compiled content.
- Writes modify the physical source file.
- Metadata operations apply to the physical source file when meaningful.
- Store sync remains required after write/create/truncate/rename/unlink.

## 3. File Handle Semantics

Phase 1 introduces a handle table.

Each open file handle must record:

- handle id;
- virtual path;
- resolved physical path if any;
- path type;
- real `std::fs::File` for passthrough paths;
- open flags;
- readable/writable/append booleans;
- whether the handle represents virtual compiled content.

Required behavior:

- `open` on physical files creates a real fd-backed handle.
- `create` creates and opens a real fd-backed handle.
- `read` and `write` prefer fd-backed operations over reopening by path.
- `release` removes the handle.
- `flush` and `fsync` operate on the handle's fd when one exists.

Virtual compiled reads may remain stateless, but write opens on read-only virtual paths must fail.

## 4. Open And Create Flags

Phase 1 must honor these flags for physical passthrough files:

| Flag | Required behavior |
|------|-------------------|
| `O_RDONLY` | read allowed, write rejected |
| `O_WRONLY` | write allowed, read rejected |
| `O_RDWR` | read and write allowed |
| `O_CREAT` | create missing regular file |
| `O_EXCL` | with `O_CREAT`, fail if path exists |
| `O_TRUNC` | truncate writable file on open and sync store for `SKILL.md` |
| `O_APPEND` | writes append regardless of FUSE write offset |
| `O_DIRECTORY` | fail if target is not a directory |
| `O_NOFOLLOW` | fail on symlink where the platform exposes that distinction |

For unsupported or invalid combinations, return a deterministic POSIX errno instead of silently accepting the operation.

## 5. Read And Write

Physical passthrough reads:

- use the open handle fd;
- read from the requested offset;
- return EOF with empty data when offset is beyond file size;
- do not read the entire file into memory.

Physical passthrough writes:

- use the open handle fd;
- honor append mode;
- return the exact byte count written or the preserved errno;
- trigger `SyncEvent::Reparse` when the path is `SKILL.md`.

Compiled `SKILL.md` reads:

- continue to return `compiler::compile(raw, env_profile)`;
- report file size as compiled byte length for virtual read consistency;
- do not expose raw frontmatter through the mounted read path.

## 6. Metadata

`getattr` should project physical metadata for passthrough paths using Unix metadata fields when available.

`setattr` must handle:

- `size`: truncate physical file; reparse when `SKILL.md`;
- `mode`: chmod physical path;
- `uid/gid`: chown physical path where permitted;
- `atime/mtime`: update physical timestamps;
- unsupported fields: return success only if safely ignored by platform convention, otherwise return an explicit errno.

Virtual paths:

- root, `/skills`, and skill dirs expose deterministic directory attrs;
- `skill-discover/SKILL.md` exposes deterministic read-only file attrs;
- metadata mutations on read-only virtual paths fail.

## 7. Directory Semantics

Phase 1 must implement stable directory handles:

- `opendir` creates a sorted snapshot of entries and returns a directory handle.
- `readdir` uses snapshot offsets from the handle.
- `releasedir` frees the handle.
- `fsyncdir` succeeds for physical directory handles when the source filesystem supports sync.

Snapshot rules:

- An already-open directory stream is stable even if the underlying directory changes.
- A newly opened directory stream sees the latest view/store/physical state.

## 8. Sync And Filesystem Info

`flush`:

- should surface pending write errors if tracked;
- may be a no-op success for read-only virtual handles.

`fsync`:

- calls `sync_all` or `sync_data` on the real fd depending on `datasync`;
- returns accurate errors.

`fsyncdir`:

- syncs physical directory fd when available;
- returns deterministic success or unsupported error for pure virtual dirs.

`statfs`:

- returns non-zero filesystem statistics from the source filesystem;
- must not keep the current all-zero default.

## 9. Access

`access` must support:

- `F_OK`;
- `R_OK`;
- `W_OK`;
- `X_OK`;
- combinations of the above.

For physical paths, check against source metadata and caller identity or delegate to the platform where possible.

For virtual paths:

- read access succeeds for visible virtual dirs and files;
- write access fails for read-only virtual paths;
- execute/search access succeeds for visible virtual directories.

## 10. Rename

Plain rename remains supported and must preserve existing store sync behavior.

Phase 1 must stop ignoring rename flags:

- unknown flags return an explicit error;
- `RENAME_NOREPLACE` must either be implemented correctly or rejected without replacing the target;
- `RENAME_EXCHANGE` must either be implemented correctly or rejected without changing either path.

Skill directory rename invariants remain mandatory:

- old name disappears immediately;
- new name appears immediately;
- stale frontmatter does not resurrect the old store key.

## 11. Error Policy

When an operation maps to a physical filesystem syscall, return the syscall errno if available.

Use explicit virtual-path errors:

| Situation | Preferred errno |
|-----------|-----------------|
| path not found | `ENOENT` |
| create existing with exclusive create | `EEXIST` |
| directory required but file found | `ENOTDIR` |
| file required but directory found | `EISDIR` |
| non-empty directory removal | `ENOTEMPTY` |
| write to read-only virtual path | `EROFS` or `EACCES`, but choose one consistently |
| unsupported operation | `ENOSYS`, `ENOTSUP`, or `EOPNOTSUPP` depending on platform convention |
| invalid flags | `EINVAL` |

Avoid converting all unexpected errors to `EIO` unless no better error exists.

## 12. Acceptance Tests

Create or extend FUSE integration tests to cover:

- open/read/write with read-only, write-only, read-write;
- append write behavior;
- create exclusive behavior;
- truncate through open and `setattr`;
- chmod and timestamp mutation;
- access checks;
- statfs non-zero output;
- fsync/flush success on passthrough handles;
- stable directory stream offsets;
- rename flag validation;
- SKILL.md store sync after write/truncate/rename/unlink;
- normal and in-place mode parity for core Phase 1 cases.

The test suite must skip gracefully when FUSE is unavailable.

## 13. Phase 1 Exit Criteria

Phase 1 is complete when:

- existing tests still pass;
- `cargo test -p skillfs-fuse --test write_guard_tests` passes on a FUSE-enabled Linux runner;
- `cargo test -p skillfs-fuse --test posix_phase1_tests` passes on a FUSE-enabled Linux runner;
- P0 rows in `POSIX_FS_TEST_MATRIX.csv` are covered by tests or explicitly marked deferred;
- this spec is updated to reflect any accepted deviations.
