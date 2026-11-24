# Lua-LSM file backend

[中文版](README_zh.md)

This Lua-LSM backend shares Cedar file-policy lowering with AgentSight and
deploys independent Lua modules directly through securityfs. It makes no
AgentSight requests. The kernel baseline is Lua-LSM
`92a8552ad51490433034423ef33c3f61fcb3cdb9`, ABI version 1. Native Client tests for the earlier inode-label template passed on Alibaba Cloud Linux 4, kernel
`6.6.102-008.0.2.ali6000.pro.alnx4.x86_64`. On the same host, an isolated daemon
deployed ten Bindings and confirmed their protection after an orderly restart.
The runtime path template and packaged systemd deployment still need native verification. A later full daemon test
on the host ended in a kernel crash during module removal. The Lua-LSM owner
identified a known kernel issue and is backporting the fix. Verification on a
kernel with the backport is pending.

## Configuration and lifetime

Set `AGENT_SEC_FILE_BACKEND=lua-lsm` in the daemon environment. The default remains
`agentsight`; other values reject startup. Existing deployments retain their saved
route. Gateway credential delivery is unchanged. The current system unit needs an
administrator-managed drop-in, validated on the target distribution:

```ini
[Service]
Environment=AGENT_SEC_FILE_BACKEND=lua-lsm
CapabilityBoundingSet=CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER CAP_SYS_PTRACE CAP_MAC_ADMIN
ReadWritePaths=/sys/kernel/security/lua
```

`CAP_MAC_ADMIN` must be effective in the initial user namespace. The Client owns
private state under `/var/lib/agent-sec/lua-lsm`; preserve it with the Policy
database. Modules survive daemon stops. When discovery observes a root process
exit or an administrator deletes its Scope, the daemon schedules Binding cleanup.
Surviving descendants lose that Binding's protection when the module unloads.
Deleting a Policy Template keeps existing frozen Scope snapshots.

## Implementation

Production code follows the same Translator/protocol/Client split as AgentSight:

- `asc-policy-translator-lua-lsm` lowers Bindings to Lua artifacts without I/O.
- `asc-lua-lsm-protocol` owns the internal plan, module names and canonical Lua
  template encoding, shared by generation and Client source-integrity checks.
- `asc-lua-lsm-client` owns securityfs I/O, initialization, recovery and cleanup. It
  has no Translator or Cedar dependency.
- `asc-file-policy` contains backend-neutral data types; shared Cedar lowering
  lives in `asc-policy-cedar::file_protection`.

`asc-lua-lsm` retains compatibility exports and integration-test commands. The
daemon uses the concrete crates directly. The internal plan is not a kernel ABI:
`register` receives Lua source, and `unregister` receives the module name.

Lua-LSM accepts only Cedar policies. A native SecCore template is rejected during
translation with `LUA_LSM_CEDAR_REQUIRED`.

One template implements independent read, ordinary-write and delete protection
for exact regular files, directory trees and path patterns, with target-owned
exclusions. Unsupported policy constructs reject the entire translation. The
SecCore AgentSight Translator accepts exact-file and directory-tree read targets. Names are
`asc_{binding_id}_r{binding_revision}`.
PAP/Scope/Binding wire fields and the four reconciler Client methods are unchanged.

On Lua-LSM, ordinary-write denial covers content changes and creation of a matching
directory entry by the subject, including a regular file, directory, symlink,
special file, hard link or rename destination. This also covers atomic replacement
of an exact file. Exclusions still apply. The schema distinguishes `create`
from `write`. A standalone create-only restriction is unsupported here. AgentSight
has its own enforcement contract. A write policy requires 18 recognized hooks,
including the path hooks for directory entries and both truncate hooks. A kernel
missing required hooks cannot confirm a deployment.

The Cedar condition `resource.path like "/data/*.txt"` covers both
`/data/a.txt` and `/data/nested/a.txt`.
Cedar's star spans separators, its question mark is literal, and `\*` is a
literal star. For `resource.path like "/data/*" && resource.path like "*.txt"`,
the Lua-LSM Translator scans `/data` and Lua evaluates both predicates. Each
alternative must have a positive condition bounding its scope in SecCore's mount
view. A rule can be installed before its file or directory exists. Each operation
matches its current path, including newly created entries and files that replace
an earlier object at the same name. Exact exclusions apply only to that entry.
Subtree exclusions also cover its descendants.

Deployment rejects an overlayfs target or an overlayfs submount inside a protected
tree with `LUA_LSM_OVERLAYFS_UNSUPPORTED`. This applies to empty directories and
all file operations. For a missing target, the nearest existing ancestor determines
the mount. A separate tmpfs mount inside an overlay root can still be protected.
The check applies on every kernel because existing mappings on some overlayfs
mounts report a different device and inode from the target path.

For example, blocking writes with `resource.path like "/work/secrets/*"` covers
`/work/secrets/key` even after the kernel evicts and reloads its inode.
An existing hard link at `/tmp/key` does not prevent deployment. Reads, writes
and unlink through that outside name are outside the protection guarantee.
An in-scope name continues to match even when an outside link exists. A scoped
process may also be denied when creating a new hard link across the boundary.

The Client resolves existing symlinks in the target or its ancestors at
registration. A dangling target symlink is rejected. Redeploy after retargeting
a symlink. A new symlink inside a tree does not pull an external referent into
scope. Cedar `like` compares ordered literal fragments.
Each pattern allows up to 4,096 bytes and 64 unescaped wildcards.

Mixed Cedar Bindings share one validated PST: the selected file Translator consumes
the file projection, and Gateway consumes the network projection. All plans prepare
before mutation. Binding cleanup covers both deployments.

`prepare_apply` only validates internal input. `create` writes complete Lua source
to `register` in one syscall. `update` confirms the new version before unregistering
old names. The old modules stay registered if initialization fails. `delete` writes
saved names to `unregister`. Short writes, `EBUSY` and ambiguous existing modules
remain Unknown. Synchronous calls are not abandoned while still modifying state.

The Client saves a local deployment record before writing `register`. A full-length
write confirms synchronous kernel registration. The Client reads `modules` to
check the module name and recognized hook count (`nlsm`). It reports Present after
initialization completes and the completion record is saved. During updates the
Client completes the new deployment before retiring old modules and checks that
the new name is still listed after retirement.

After a full-length `unregister` write or `ENOENT`, the Client reads `modules`.
It reports Absent only when the listing succeeds and omits the requested name.
Failed readback leaves the target Unknown, so reconciliation cannot report a
successful deployment or deletion from an unverified result.

After registration the Client seeds the verified root and existing descendants.
On access or fork, a caller can establish membership through its current ancestry
when its PID namespace resolves the saved PID to the marked root. Fork and exec
preserve membership. Private commands require initial-user-namespace
`CAP_MAC_ADMIN` and acknowledge with `EALREADY`. File hooks start checking
paths as soon as a process becomes a member. Failed initialization may leave
partial protection until retry or cleanup.

The template binds `current` to the module environment before reading task
properties. File and directory inodes carry no policy state.

Mapping preflight matches `/proc/<pid>/maps` device/inode identities against the
included files. During a first deployment, the final check covers surviving processes
seen in either scan, including descendants seen in the first scan and later reparented
outside the tree. The client keeps the initial snapshot in memory for that deployment
attempt. A descendant that leaves the tree before either scan sees it and has no
membership label can remain unprotected even when the Binding reaches `Ready`.
Keep the process tree stable during first deployment when all descendants must be
protected. Updates check members of the old module.
Read protection rejects existing file mappings, including private and PROT_NONE
mappings. Write protection rejects existing shared writable mappings. Inspection
failures block deployment. Existing writable FDs and shared read-only mappings are
accepted. Subsequent writes and shared write upgrades go through file hooks. A new mapping can appear between a process's snapshot and the point at
which the relevant hook begins denying access. AgentSight preflight has the same
snapshot limitation.

Updates transfer members from the old modules rather than rediscovering ancestry.
Atomic membership/exit counters fence concurrent forks and exits before an old
module can be removed, including orphaned descendants and threads. A persisted
registration-instance token prevents reuse of transfer markers after re-registration.
Incomplete transfer keeps the old protection and uses the existing retry policy.

The deployment record includes the boot ID, the digest of the source and other
deployment inputs including process identity, and whether initialization completed.
A same-boot daemon restart preserves Ready Bindings and both deployment receipts
of a mixed Binding. Pending work resumes. Interrupted apply/delete follows the
existing retry budget. When an apply is retried, the Client checks the native module
listing against the record. A matching completed deployment skips initialization.
A matching incomplete deployment repeats initialization on the registered module.
A missing module is reapplied to the original verified process. Names without
matching evidence are never overwritten or adopted. Delete invalidates evidence
before unregister. Template changes require a new revision or explicit cleanup.

## Boundaries requiring attention

- The existing-mapping preflight visits at most 65,536 entries per scan. Workloads can run between
  discovery and the point when their protection takes effect.
- File hooks use the resolved namespace path. Directory-entry hooks receive a
  filesystem-relative dentry path and filesystem type. The Client translates it
  using the mount layout at registration. Separate superblocks of the same type
  with identical relative paths can cause conservative denials. Existing bind
  aliases in the verified root's mount view are included. New aliases, namespace
  changes, chroot and mount replacement require a fresh deployment for coverage.
  Procfs must match the daemon's PID view.

- First deployment, or recovery without a confirmed old module, requires the
  original root. Lost kernel ownership cannot be reconstructed from a reused PID.
  Concurrent fork/exit behavior has deterministic fixture coverage; sustained
  kernel stress and namespace combinations remain unverified.
- **Known issue with PID reuse.** During deployment or membership transfer, Lua
  labels existing descendants by PID. If the PID is reused after SecCore checks
  the process identity, Lua can label an unrelated task. A failed identity check
  after labeling leaves the label in place, so the Binding can restrict that task
  and its future children. Resolving this requires a Lua-LSM API to verify the
  expected process instance on the same task object that receives the label.
- Moves and new links that cross read/write scopes, directory moves involving
  filters, and mount operations over protected entries or their ancestors may
  be denied conservatively.

- Existing mappings and already-read data cannot be revoked. Template exceptions
  deny with distinct audit reasons; errors before callback entry retain kernel
  behavior. Auditing is best effort.
- Native APIs do not expose LIVE state, content digests or CAS. Local confirmation
  assumes one trusted SecCore writer; absent evidence is not guessed.
- io_uring/AIO, cross-process memory access and asynchronous delegation are not
  part of the validated read/write contract.

## Coverage against AgentSight

The comparison uses the current checkout's
[read/runtime acceptance tests](../../../../agentsight/crates/agentsight-enforcer/tests/inode_runtime.rs),
[write tests](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_write_tests.rs),
[delete tests](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_delete_tests.rs),
[rename tests](../../../../agentsight/crates/ebpf-ifc-engine/src/file_protection_rename_tests.rs)
and [mapping preflight](../../../../agentsight/crates/agentsight-enforcer/src/actplane/preexisting_maps.rs).
Those AgentSight suites were inspected, not rerun in this work. Lua evidence below
lists scenarios in the default-ignored [native Client tests](tests/client.rs).
The recorded live runs used the earlier template.

| Area | Lua-LSM test scenarios | Comparison or remaining boundary |
| --- | --- | --- |
| Independent permissions | Read-only, write-only and delete-only restrictions on files/trees; other operations, unrelated paths/processes and detach remain usable | AgentSight requires `directory_tree_read_protection` for directory-tree read denial |
| Read entry points | Existing RW FD, pread/readv/preadv, dup, same-process SCM_RIGHTS, sendfile/splice/copy_file_range, symlink, private/shared/execute mappings, PROT_NONE upgrade and executable loading | Baselines before apply and after detach distinguish policy denial from environment failure; cross-process SCM_RIGHTS passed in an isolated QEMU guest, but host-kernel coverage remains pending |
| Write entry points | Existing RW FD, write/pwrite/writev/pwritev, truncate/ftruncate/O_TRUNC, hole punching, copy_file_range, shared writable mmap and mprotect | Denial must preserve bytes; private COW, private upgrades, reads and chmod remain usable |
| Existing mappings | Reject private/shared/PROT_NONE read mappings and shared writable mappings; preserve old deployment on failed update; retry after unmap | AgentSight additionally rejects writable FDs/shared mappings conservatively; Lua mapping hooks allow the tested narrower preflight |
| Deletion and namespace changes | unlink/rmdir, move-out, overwrite, exchange in both directions, ancestor moves and same-tree rename under delete-only protection | Write protection also rejects creation of a matching rename destination |
| Resource identity | Hard-link aliases, symlink reads, newly created tree members and excluded subtree controls | Outside hard-link names are outside the Lua path policy. A matching name is protected independently |
| Subject identity | Existing child, future thread, fork/exec/setsid, update of an orphan while the module is still loaded, and a child in a separate PID namespace | Direct Client tests leave cleanup to the caller. The daemon schedules cleanup on root exit. User namespaces and sustained churn remain unverified |
| Mount view | Separate mount namespace, a pre-existing bind alias, denied bind over a protected root and an allowed unrelated bind | Host filesystem view remains unchanged; move_mount/remount, stacking and chroot/pivot_root need more coverage |
| Privilege boundary | Protection survives setuid; same-UID unrelated process retains access; unprivileged unload/private-control access fails | Full systemd capability/mount setup remains unverified |
| Binding isolation | Read/write Bindings overlap on one inode; deleting either preserves the other; same-name re-registration after cleanup | Concurrent update/delete and interrupted cleanup still need stress tests |
| Recovery | Same-boot Client confirmation, successful and failed updates, explicit cleanup and orderly daemon restart with ten active Bindings | Crash recovery and database/state-directory fault injection remain pending |

Before production acceptance, add user-namespace, chroot/pivot_root and more mount-operation tests;
cross-Binding exclusions/overlapping trees, replacement and deep/non-ASCII paths;
fork/exit/mapping/update races; live audit/runtime-error evidence; and a complete
packaged systemd deployment and restart test. Filesystem variants such as tmpfs
and network filesystems need separate acceptance. Lua callback/VM faults
outside the template retain the kernel's existing failure behavior.

## Verification and rollback

From `src/agent-sec-core/v2`:

```sh
cargo test -p asc-lua-lsm -p asc-lua-lsm-client -p asc-policy-translator-agentsight
LUA_BIN=/path/to/lua5.1 cargo test -p asc-lua-lsm lua51_template_behavior -- --ignored
```

Rust tests cover request bytes, errors, update ordering and restart evidence.
Local procfs regressions also check mapped descendants, file/tree identities,
outside hard-link mappings and excluded mappings without modifying kernel state.
`tests/template.lua` runs in Ubuntu CI with Lua 5.1 and mocked kernel objects,
including Cedar path matching. Repository and daemon tests cover
root-exit cleanup and recovery. Native enforcement is tested manually on a
disposable kernel host with the following command. Set `TMPDIR` to a writable
non-overlay filesystem that allows execution for the native exec and mmap tests.

```sh
cargo test -p asc-lua-lsm live_kernel_ -- --ignored --test-threads=1
```

The earlier inode-label template passed all 15 native groups on the ABI 1 host on 2026-11-24. They cover a
missing file and tree with shared future ancestors, first creation without a
residual directory entry, atomic replacement, hard links, symlinks, FIFOs,
native glob and Cedar `like` enforcement, exclusions and mapping preflight. These groups
exercise the Translator and Client directly. A separate daemon run deployed five
Bindings of each syntax, checked that protection survived an orderly restart,
and confirmed that removing the Bindings restored access.
Before downgrading to a daemon without a Lua-LSM Client, delete the owning
Scopes with the current daemon. Confirm that cleanup finished and each recorded
module name is absent from `/sys/kernel/security/lua/modules`. Preserve the
policy database and Client state together. Changing the default backend leaves
loaded modules enforcing.

If the downgrade has already happened, restore a compatible daemon to complete
cleanup. For emergency manual unload, an administrator with effective
`CAP_MAC_ADMIN` in the initial user namespace can set `module` to the exact
name from the affected Binding's cleanup receipt and run the following commands.
Verify that the exact name disappeared. A write error or a remaining entry
leaves cleanup unconfirmed. Use a compatible daemon to reconcile the saved receipt.

```sh
printf '%s' "$module" > /sys/kernel/security/lua/unregister
cat /sys/kernel/security/lua/modules
```

V1 relationship: new V2 backend; V1 Python behavior is unchanged. Internal changes:
shared intent, direct Client, selection and recovery. Existing Client ports and
persisted TargetRef shape remain compatible.
