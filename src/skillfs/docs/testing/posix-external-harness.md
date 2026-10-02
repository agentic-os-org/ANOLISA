# SkillFS POSIX External Test Harness (Packages T0 + T0.1)

**Status**: Optional. Not invoked by `cargo test` or `scripts/test.sh`.
**Script**: [`scripts/posix/run_pjdfstest.sh`](../../scripts/posix/run_pjdfstest.sh)
**Manifests** (three buckets, each populated against an upstream
citation — see ["Adding to a manifest"](#adding-to-a-manifest)):
- [`scripts/posix/expected_fail.txt`](../../scripts/posix/expected_fail.txt)
  — direct unsupported surface (link / symlink / mkfifo / mknod /
  posix_fallocate / chflags).
- [`scripts/posix/blocked_dependent.txt`](../../scripts/posix/blocked_dependent.txt)
  — test files whose own setup uses an unsupported surface
  (`create_file fifo|block|char|socket`, internal `symlink`,
  hardlink "multiply linked").
- [`scripts/posix/caller_identity.txt`](../../scripts/posix/caller_identity.txt)
  — test files that require POSIX semantics SkillFS does not
  currently emulate: per-caller uid/gid fidelity (`pjdfstest -u <uid>
  -g <gid>`), sticky-bit (`S_ISVTX`) enforcement on rename/rmdir,
  and `lchmod` probes.

**Upstream**: <https://github.com/pjd/pjdfstest>

## What T0 Is

T0 drives an external POSIX conformance suite (pjdfstest) against a
real SkillFS FUSE mount. T0.1 refined the harness to make the report
actionable (profiles, two manifests, three buckets, verbose rerun) and
fixed three small POSIX gaps the T0 baseline surfaced (`create` /
`mkdir` mode+umask; passthrough `getattr` inode). The harness itself
changes no filesystem behavior. It:

1. Builds the `skillfs` binary (debug by default; `--release` for the
   release binary).
2. Creates a temporary source/mount pair and a single normal skill
   (`harness-skill`) with a writable passthrough subdirectory
   (`sandbox/`).
3. Mounts SkillFS over that source in the default non-in-place layout.
4. Selects a subset of pjdfstest's `tests/**/*.t` files based on the
   `--profile` / `--include` / `--exclude` rules below, then runs
   `prove` against that selection with cwd set to
   `skills/harness-skill/sandbox/` so every test creates its temp files
   inside the SkillFS passthrough sandbox.
5. Classifies each failing pjdfstest file into one of three buckets
   (see ["Interpreting the Report"](#interpreting-the-report)).
6. Optionally re-runs the unexpected bucket with `prove -v` for
   investigation.
7. Tears the mount down via `fusermount3 -u` and removes the temp tree
   (pass `--keep` to retain it).

The sandbox path was chosen deliberately to keep the harness out of
all SkillFS-specific surfaces: it never reads or writes
`skill-discover/SKILL.md`, `<skill>/SKILL.md`, `.skill-meta/`, or
lifecycle reserved roots (`.staging`, `.certified`, `.quarantine`,
`.archive`). That keeps the harness a pure POSIX passthrough probe.

## Why pjdfstest Is Not a Cargo Dependency

pjdfstest is an external C/autotools project that must be built on the
target host and **requires root** to run. Pulling it into `cargo test`
would make the in-tree test matrix depend on root, a clean autotools
chain, and a built C binary that does not ship with this repo. The
harness keeps the dependency optional: if `prove`, `fusermount3`,
root, or the pjdfstest checkout are missing, it exits with code `2`
and a single actionable hint per missing prerequisite.

## Host Prerequisites

Linux only (pjdfstest does not target macOS as a host). Install once:

```sh
# Build and runtime deps for pjdfstest itself.
sudo dnf install -y autoconf automake gcc make perl perl-Test-Harness fuse3
# Or, on Debian/Ubuntu:
# sudo apt install -y autoconf automake gcc make perl perl-modules fuse3
```

You also need write access to `/dev/fuse`. On most images, root has
this by default.

## Installing pjdfstest

```sh
git clone https://github.com/pjd/pjdfstest.git ~/pjdfstest
cd ~/pjdfstest
autoreconf -ifs
./configure
make pjdfstest
```

The build produces an executable named `pjdfstest` at the repo root.
The test scripts under `tests/` walk upward to find it, so we just
need to hand the harness the checkout path.

## Running the Harness

```sh
# Default smoke profile: excludes both manifests, runs the rest.
sudo SKILLFS_PJDFSTEST_DIR=~/pjdfstest scripts/posix/run_pjdfstest.sh \
  --report /tmp/skillfs-smoke.txt

# Full profile: runs every *.t under tests/ (reproduces T0 baseline).
sudo scripts/posix/run_pjdfstest.sh --pjdfstest ~/pjdfstest \
  --profile full --report /tmp/skillfs-full.txt

# Verbose rerun of any residual unexpected failures.
sudo scripts/posix/run_pjdfstest.sh --pjdfstest ~/pjdfstest \
  --rerun-failures-verbose --keep
```

All flags:

| Flag                                | Effect                                                                                            |
|-------------------------------------|---------------------------------------------------------------------------------------------------|
| `--pjdfstest <PATH>`                | pjdfstest checkout (must contain built `./pjdfstest` and `./tests/`). Overrides env var.          |
| `--release`                         | Build/run the release `skillfs` binary.                                                           |
| `--profile smoke\|full`             | Default `smoke`: skip every glob from all three manifests. `full`: run every test file.           |
| `--include <PATTERNS>`              | Comma-separated globs (relative to `tests/`). When set, only matching files run. Repeatable.      |
| `--exclude <PATTERNS>`              | Comma-separated globs to exclude. Stacks with profile excludes and wins over `--include`.         |
| `--rerun-failures-verbose`          | After the main run, re-invoke `prove -v` against the unexpected-fail set into `rerun-verbose.log`.|
| `--expected-fail <PATH>`            | Override the expected-fail manifest.                                                              |
| `--blocked-manifest <PATH>`         | Override the blocked-dependent manifest.                                                          |
| `--caller-identity-manifest <PATH>` | Override the caller-identity manifest.                                                            |
| `--report <PATH>`                   | Write the final summary to PATH (also printed on stdout).                                         |
| `--keep`                            | Keep the temporary source/mount tree, mount log, prove log, and rerun log.                        |
| `--self-test`                       | Exercise the parser+classifier on canned input. No FUSE/sudo/pjdfstest required. Exits 0 / 4.     |
| `-h`, `--help`                      | Show CLI help and exit.                                                                           |

Environment overrides:

- `SKILLFS_PJDFSTEST_DIR` — same as `--pjdfstest` (CLI flag wins).
- `SKILLFS_POSIX_KEEP=1` — same as `--keep`.

### Selection order

For each `tests/**/*.t`, the harness applies, in this order:

1. `--include` globs (if any). A non-matching file is dropped.
2. `--exclude` globs. A matching file is dropped (wins over include).
3. `--profile smoke` ⇒ drop files matching **any** of the three
   manifests (expected-fail, blocked-dependent, caller-identity).
   `--profile full` ⇒ no manifest-based drop.

The remaining list is the prove input. Manifest classification of
**failing** files still happens at report time regardless of profile.

## Interpreting the Report

The harness prints a single summary block. Example with T0.1 buckets:

```
SkillFS T0/T0.1 — External POSIX Harness Report
===============================================
repo:           /home/you/code/SkillFS
skillfs build:  debug (.../target/debug/skillfs)
pjdfstest:      /home/you/pjdfstest
sandbox:        /tmp/skillfs-t0.AbCxYz/mount/skills/harness-skill/sandbox
profile:        smoke
selected:       128 / 237 test files
skipped by:     include=0 exclude=0 profile=109
prove exit:     1
prove log:      /tmp/skillfs-t0.AbCxYz/prove.log
rerun log:      /tmp/skillfs-t0.AbCxYz/rerun-verbose.log

prove summary:
  Files=128, Tests=4500, ...
  Result: FAIL

expected fails (unsupported surface): 0

blocked fails (depends on unsupported helper): 0

caller-identity fails (uid/gid/sticky/lchmod semantics gap): 0

unexpected fails: 8
  ! mkdir/03.t
  ! open/02.t
  ! open/03.t
  ! rename/02.t
  ! rmdir/03.t
  ! truncate/03.t
  ! unlink/03.t
  ! unlink/14.t

verdict: UNEXPECTED_FAILURES (8 real failing files; investigate before promoting to a manifest)
```

Read it as follows:

- **`prove exit`**: prove's own exit status. Non-zero whenever any
  test file fails or any subtest is `not ok`; the harness still
  reports `OK` if every failing file matched a manifest.
- **`prove summary`**: the raw `Files=…, Tests=…` and `Result:` lines
  from prove. Use them to spot wholesale regressions (e.g. test count
  collapsed to zero — usually a mount or build problem; see the prove
  log).
- **expected fails (unsupported surface)**: failing test files whose
  relative path matches a pattern in `expected_fail.txt`. Each
  corresponds to a SkillFS surface we deliberately have not
  implemented yet (symlink creation, hard links, mkfifo/mknod,
  fallocate, BSD-only `chflags/`).
- **blocked fails (depends on unsupported helper)**: failing test
  files matching `blocked_dependent.txt`. pjdfstest's own setup
  inside these files calls an unsupported helper (`create_file
  fifo|block|char|socket`, internal `symlink`/`mkfifo`/`mknod`,
  hardlink "multiply linked"), so the file cannot run end to end on
  SkillFS until that surface lands.
- **caller-identity fails (uid/gid/sticky/lchmod semantics gap)**:
  failing test files matching `caller_identity.txt`. The targeted op
  may work, but pjdfstest's subtests assert behavior under
  `-u <uid> -g <gid>`, sticky-bit owner mismatch, or `lchmod`
  semantics that the SkillFS FUSE daemon does not currently
  emulate. Lands when a future package wires `req.uid()` /
  `req.gid()` through to the underlying syscall (e.g. via FUSE
  `default_permissions` plus per-call seteuid/setegid in worker
  threads).
- **unexpected fails**: failing test files matching none of the
  three manifests.
  Each is a real signal — either a SkillFS regression on a surface we
  believe is supported, or a host/filesystem condition that the
  manifest needs to acknowledge explicitly. **Investigate before
  expanding either manifest** — see the manifest-citation rule below.
  Use `--rerun-failures-verbose` to get a `prove -v` of just these
  files into `rerun-verbose.log`.

`verdict: OK` ⇒ exit `0`. `verdict: UNEXPECTED_FAILURES` ⇒ exit `3`.
Any setup error (missing pjdfstest, missing prove, not root,
`/dev/fuse` absent, build failure, mount timeout) ⇒ exit `2` with a
one-line hint.

## Adding to a manifest

All three manifests use the same syntax — one glob per line, `#`
for comments. A glob is matched against the test file's relative
path under `tests/` (e.g. `link/00.t`, `chmod/*`).

**Hard rule (applies to all three manifests)**: each glob must be
immediately preceded by a `#` comment naming the upstream pjdfstest
file and the exact dependency reason. "It failed in the baseline" is
**not** a citation — entries without a concrete upstream pointer are
not allowed. If a failure cannot be cited, it stays in the unexpected
bucket and surfaces in the verbose rerun for follow-up. This rule
exists so a future maintainer can re-audit the manifest after a
SkillFS feature lands and know exactly which entries to remove.

Reverse audit: when a Phase 2 package implements one of the
expected-fail surfaces (symlink creation, hardlink, mkfifo/mknod,
fallocate), remove the corresponding entry from `expected_fail.txt`
and re-run the harness — failures that used to be expected become
real signal at that point. Likewise, when an unsupported-helper
surface ships, audit `blocked_dependent.txt`; when per-caller
uid/gid fidelity lands, audit `caller_identity.txt`. Every removed
entry should let the harness surface the same files in the
unexpected bucket, where they get re-investigated.

## Self-test

The harness ships a `--self-test` mode that exercises the prove-output
parser and the manifest classifier on canned input. It runs locally
without FUSE, sudo, or a pjdfstest binary and is useful for
regression-checking the parse path:

```sh
bash scripts/posix/run_pjdfstest.sh --self-test
```

Expected output: four `ok:` lines (one per canonical bucket) and
`[self-test] OK`. Exit code `0` on success, `4` on classification
mismatch.

## What This Harness Does Not Cover

By design, the external harness only measures and classifies behavior. It does
not implement filesystem features. Current support status for symlink creation,
hard links, FIFO creation, xattrs, sparse-file APIs, and copy acceleration is
tracked in `docs/skillfs-filesystem-capability-record.md` and
`POSIX_FS_TEST_MATRIX.csv`.
- Changing SKILL.md compiled read/write semantics, skill-discover
  semantics, `.skill-meta` protection, or lifecycle namespace policy.
  The sandbox path is chosen specifically so pjdfstest never crosses
  those boundaries.
- Adding pjdfstest as a cargo or CI dependency. Normal `cargo test`
  and `scripts/test.sh` must continue to pass without pjdfstest.
- Caller-identity fidelity in the FUSE filesystem (the `pjdfstest -u
  <uid> -g <gid>` switches). Several `truncate/`, `unlink/`,
  `chown/` files exercise permission semantics that depend on the
  FUSE daemon honoring per-caller uid/gid; that is bigger than T0.1.
