# POSIX Filesystem References For SkillFS

This file collects standards, implementation anchors, and test references used
for SkillFS filesystem compatibility work.

## Standards

- The Open Group, [Single UNIX Specification Version 5 overview](https://www.unix.org/overview.html)
  - Confirms the 2024 Single UNIX Specification is based on POSIX.1-2024 / Base Specifications Issue 8.
  - Useful sections: standards alignment, file access, error numbers, feature test macros.

- The Open Group, [POSIX.1-2024 System Interfaces index](https://pubs.opengroup.org/onlinepubs/9799919799/idx/functions.html)
  - Canonical online index for APIs to map into the SkillFS test matrix.
  - Filesystem-relevant APIs include `access`, `chmod`, `chown`, `close`, `creat`, `faccessat`, `fchmod`, `fchown`, `fcntl`, `fdatasync`, `fsync`, `fstat`, `fstatat`, `fstatvfs`, `ftruncate`, `futimens`, `link`, `linkat`, `lseek`, `lstat`, `mkdir`, `mkdirat`, `mkfifo`, `mknod`, `open`, `openat`, `opendir`, `posix_fallocate`, `read`, `readdir`, `readlink`, `rename`, `renameat`, `rmdir`, `stat`, `statvfs`, `symlink`, `truncate`, `unlink`, `utimensat`, `write`.

- The Open Group, [rename()/renameat()](https://pubs.opengroup.org/onlinepubs/9799919799/functions/rename.html)
  - High-value reference for atomic namespace semantics, trailing slash behavior, replacement behavior, symlink behavior, and required errno cases.

## Local Code Anchors

- `README.md`
  - Current product summary and feature matrix.

- `docs/specs/skillfs-v1-spec.md`
  - Current architecture snapshot and write/store synchronization flow.

- `docs/specs/core-spec.md`
  - Parser, store, views, compiler, env, watcher overview.

- `docs/specs/fuse-spec.md`
  - FUSE public API, mounted layout, behavior contracts, rejected operations.

- `docs/skillfs-filesystem-capability-record.md`
  - Maintained record of filesystem capabilities, security integration surfaces,
    test coverage, boundaries, and implementation history.

- `crates/skillfs-core/src/parser.rs`
  - `SKILL.md` parsing, fallback metadata behavior, parse status model.

- `crates/skillfs-core/src/store.rs`
  - Flat/categorized source loading, store CRUD, primary/secondary split.

- `crates/skillfs-core/src/views.rs`
  - `skillfs-views.toml` load/save and default/secondary view selection.

- `crates/skillfs-core/src/watcher.rs`
  - Existing watcher module. It is tested but not connected to CLI/FUSE runtime.

- `crates/skillfs-fuse/src/lib.rs`
  - Current FUSE implementation.
  - Important local sections: `PathType`, `InodeManager`, `spawn_sync_worker`, `SkillFs`, `impl Filesystem for SkillFs`.

- `crates/skillfs-fuse/tests/write_guard_tests.rs`
  - Existing FUSE integration tests for read, write passthrough, mkdir, rename, in-place mode, stale frontmatter, and rejected operations.

- Installed `fuser` crate source, when available in the local Cargo registry:
  - Use the `Filesystem` trait as the callback checklist. Unimplemented
    callbacks in SkillFS inherit fuser defaults.

## Suggested Test Tooling

- Rust integration tests using `tempfile`, `libc`, and `std::os::unix` APIs.
- Shell smoke tests for common tools:
  - `stat`, `touch`, `chmod`, `mkdir`, `rmdir`, `ln`, `ln -s`, `mv`, `cp`, `truncate`, `sync`.
- Python or C helper binaries for exact syscall flag coverage:
  - `openat` with `O_APPEND`, `O_EXCL`, `O_DIRECTORY`, `O_NOFOLLOW`.
  - `renameat2` for Linux rename flags.
  - `utimensat`, `faccessat`, `statvfs`, `getxattr/listxattr`.
- Optional external suites when CI supports FUSE:
  - fstest/pjdfstest-style POSIX filesystem tests.
  - xfstests subset only after the filesystem grows beyond basic passthrough semantics.

## Policy Decisions To Record Before Implementation

- Whether `SKILL.md` should remain a virtual compiled file for all readers, or whether tooling should get a raw-source escape hatch.
- Whether `skill-discover` is read-only forever.
- Whether symlinks/hardlinks are allowed inside skill directories.
- Whether special files such as FIFO/device/socket nodes are allowed in images.
- Whether xattrs are required or explicitly unsupported.
- Whether default image support is Linux-only for the first rollout.
- How categorized source layouts should behave when users create or rename skills through the mount.
