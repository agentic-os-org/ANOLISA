# Lua-LSM file backend

[中文版](README_zh.md)

This Lua-LSM backend shares Cedar file-policy lowering with AgentSight and
deploys independent Lua modules directly through securityfs. It makes no
AgentSight requests. The kernel baseline is Lua-LSM
`92a8552ad51490433034423ef33c3f61fcb3cdb9`, ABI version 1. Native Client enforcement
tests pass on Alibaba Cloud Linux 4, kernel
`6.6.102-008.0.2.ali6000.pro.alnx4.x86_64`. On the same host, an isolated daemon
deployed ten Bindings and confirmed their protection after an orderly restart.
Packaged systemd deployment still needs verification. A later full daemon test
on the host ended in a kernel crash after module removal began; its cause is
unresolved.

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

One template implements independent read, ordinary-write and delete protection
for exact regular files, directory trees and path patterns, with target-owned
exclusions. Unsupported policy constructs reject the entire translation. The
SecCore AgentSight Translator accepts exact-file and directory-tree read targets. Names are
`asc_{binding_id}_r{binding_revision}`.
PAP/Scope/Binding wire fields and the four reconciler Client methods are unchanged.

Ordinary-write protection checks the new entry name before creating a regular file
or subdirectory. Template tests verify that both creation hooks reject matching
names and allow exclusions. Whether the revised 17-hook template prevents a
directory entry from appearing still needs a host-kernel retest. A resident write
policy from the 15-hook template keeps its original behavior until replaced
with a new revision. The Client requires all 17 hooks before confirming a new
write policy.

Native file rules accept `*` and `?` within one path component and a whole
`**` component across zero or more directories. For example, `/data/*.txt`
covers `/data/a.txt`; `/data/**/*.txt` also covers `/data/nested/a.txt`.
In a Cedar rule, `resource.path like "/data/*.txt"` covers both files:
Cedar's star spans separators, its question mark is literal, and `\*` is a
literal star. For `resource.path like "/data/*" && resource.path like "*.txt"`,
the Lua-LSM Translator scans `/data` and Lua evaluates both predicates. Each
alternative must have a positive condition bounding its scope in SecCore's mount
view. The scan root must exist, and the initial match set may be empty. The existing
65,536-object initialization limit applies to all entries scanned below that prefix.

The generated Lua module evaluates predicates during initialization and when entries
are created or linked. Matches follow the inode through hard links. Labels are
retained for the module lifetime. A scoped task can be denied a rename or link
that would change protection, including a directory move that would require
relabeling descendants. If another process relocates a populated directory,
redeploy to evaluate the new paths of existing descendants. During initialization,
the Client checks existing mappings against the inode matches reported by Lua.

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

After registration, the Client pins filesystem objects with `O_PATH` in SecCore's
view and opens private command files. Template callbacks use `/proc/self/fd`
references to label exact kernel inodes, preserving hard-link identity across
filesystems. The Client labels existing processes, starting with the verified root.
During file access or fork, an unlabelled caller can also establish membership
through its current ancestry if its PID namespace resolves the saved PID to that
same marked root task. Fork propagates membership and exec preserves it.
Callbacks accept commands only with initial-user-namespace `CAP_MAC_ADMIN` and
acknowledge with `EALREADY`. These commands belong to the template and run through
the file-open hook. Hooks enforce as soon as both the process and inode are marked.
Protection can therefore begin during initialization, and a failed deployment can
leave partial protection until retry or cleanup.

Object/shared caches store only scalar values. The template keeps exclusion
transitions in Lua tables and stores their numeric state IDs on inodes. It binds
`current` to the module environment with `debug.setfenv` before cache access,
because the baseline dispatcher creates that userdata without the module binding.
It uses Linux EALREADY (114) directly, which the Lua errno library does not export,
and arithmetic masks without the unavailable math library.
`inode_setattr` accepts both the tested two-argument callback and the three-argument
source-review baseline. ABI version 1 alone does not identify callback arity.

Mapping preflight matches `/proc/<pid>/maps` device/inode identities against the
included files, across the initial family and old-module members during updates.
Read protection rejects existing file mappings, including private and PROT_NONE
mappings. Write protection rejects existing shared writable mappings. Inspection
failures block deployment. Existing writable FDs and shared read-only mappings are
accepted: the live tests verify subsequent writes and shared write upgrades are
mediated. A new mapping can appear between a process's snapshot and the point at
which the relevant hook begins denying access. AgentSight preflight has the same
snapshot limitation.

Updates transfer members from the old modules rather than rediscovering ancestry.
Atomic membership/exit counters fence concurrent forks and exits before an old
module can be removed, including orphaned descendants and threads. A persisted
registration-instance token prevents reuse of transfer markers after re-registration.
Incomplete transfer keeps the old protection and uses the existing retry policy.

The deployment record includes the boot ID, the digest of the source and other
deployment inputs including process identity, and whether initialization completed.
Startup requeues Ready Lua Bindings. The Client checks the native module listing
against the record. A matching completed deployment skips process and file labelling.
A matching incomplete deployment repeats initialization on the registered module.
A missing module is reapplied to the original verified process. Names without
matching evidence are never overwritten or adopted. Delete invalidates evidence
before unregister. Template changes require a new revision or explicit cleanup.

## Boundaries requiring attention

- Bootstrap is limited to 65,536 filesystem objects. Workloads can run between
  discovery and the point when their protection takes effect.
- Filesystems beyond the acceptance host, user namespaces, chroot/pivot_root and
  additional mount operations need validation. Separate PID/mount namespaces and
  a pre-existing bind alias have live coverage. Procfs must match the daemon's PID view.
  Pre/post identity checks do not provide an atomic kernel start-time comparison.
- First deployment, or recovery without a confirmed old module, requires the
  original root. Lost kernel ownership cannot be reconstructed from a reused PID.
  Concurrent fork/exit behavior has deterministic fixture coverage; sustained
  kernel stress and namespace combinations remain unverified.
- There is no usable rename completion callback in this baseline. Some moves
  across read/write scopes and involving exclusions are rejected conservatively.
  Labels installed before link/rename may remain after failed operations.
  Mount attachment/movement at protected objects or their ancestors is also
  rejected to prevent replacing the protected topology. These behaviors are
  stricter than some AgentSight operations.
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
comes from the default-ignored [native Client tests](tests/client.rs) and the
isolated daemon restart check.

| Area | Lua-LSM live checks | Comparison or remaining boundary |
| --- | --- | --- |
| Independent permissions | Read-only, write-only and delete-only restrictions on files/trees; other operations, unrelated paths/processes and detach remain usable | AgentSight requires `directory_tree_read_protection` for directory-tree read denial |
| Read entry points | Existing RW FD, pread/readv/preadv, dup, same-process SCM_RIGHTS, sendfile/splice/copy_file_range, symlink, private/shared/execute mappings, PROT_NONE upgrade and executable loading | Baselines before apply and after detach distinguish policy denial from environment failure; cross-process SCM_RIGHTS passed in an isolated QEMU guest, but host-kernel coverage remains pending |
| Write entry points | Existing RW FD, write/pwrite/writev/pwritev, truncate/ftruncate/O_TRUNC, hole punching, copy_file_range, shared writable mmap and mprotect | Denial must preserve bytes; private COW, private upgrades, reads and chmod remain usable |
| Existing mappings | Reject private/shared/PROT_NONE read mappings and shared writable mappings; preserve old deployment on failed update; retry after unmap | AgentSight additionally rejects writable FDs/shared mappings conservatively; Lua mapping hooks allow the tested narrower preflight |
| Deletion and namespace changes | unlink/rmdir, move-out, overwrite, exchange in both directions, ancestor moves; same-tree rename remains usable | Some read/write tree moves and exclusion transitions remain more restrictive |
| Resource identity | Hard-link aliases, symlink reads, newly created tree members and excluded subtree controls | Lua protects all inode aliases; some AgentSight structured tests permit outside write/delete aliases, so the semantics differ |
| Subject identity | Existing child, future thread, fork/exec/setsid, update of an orphan while the module is still loaded, and a child in a separate PID namespace | Direct Client tests leave cleanup to the caller. The daemon schedules cleanup on root exit. User namespaces and sustained churn remain unverified |
| Mount view | Separate mount namespace, a pre-existing bind alias, denied bind over a protected root and an allowed unrelated bind | Host filesystem view remains unchanged; move_mount/remount, stacking and chroot/pivot_root need more coverage |
| Privilege boundary | Protection survives setuid; same-UID unrelated process retains access; unprivileged unload/private-control access fails | Full systemd capability/mount setup remains unverified |
| Binding isolation | Read/write Bindings overlap on one inode; deleting either preserves the other; same-name re-registration after cleanup | Concurrent update/delete and interrupted cleanup still need stress tests |
| Recovery | Same-boot Client confirmation, successful and failed updates, explicit cleanup and orderly daemon restart with ten active Bindings | Crash recovery and database/state-directory fault injection remain pending |

Before production acceptance, add user-namespace, chroot/pivot_root and more mount-operation tests;
cross-Binding exclusions/overlapping trees, replacement and deep/non-ASCII paths;
fork/exit/mapping/update races; live audit/runtime-error evidence; and a complete
packaged systemd deployment and restart test. Filesystem variants such as overlayfs,
tmpfs and network filesystems need separate acceptance. Lua callback/VM faults
outside the template retain the kernel's existing failure behavior.

## Verification and rollback

From `src/agent-sec-core/v2`:

```sh
cargo test -p asc-lua-lsm -p asc-policy-translator-agentsight
LUA_BIN=/path/to/lua5.1 cargo test -p asc-lua-lsm lua51_template_behavior -- --ignored
```

Rust tests cover request bytes, errors, update ordering and restart evidence.
Local procfs regressions also check mapped descendants, file/tree identities,
outside hard-link mappings and excluded mappings without modifying kernel state.
`tests/template.lua` executes Lua 5.1 with mocked kernel objects. Repository and
daemon tests cover root-exit cleanup and restart rechecks. On a disposable kernel
host, explicitly run the default-ignored live test:

```sh
cargo test -p asc-lua-lsm live_kernel_ -- --ignored --test-threads=1
```

All fourteen live groups passed on the ABI 1 host on 2026-11-20, including native
glob and Cedar `like` enforcement, exclusions and mapping preflight. These groups
exercise the Translator and Client directly. A separate daemon run deployed five
Bindings of each syntax, checked that protection survived an orderly restart,
and confirmed that removing the Bindings restored access.
Rollback explicitly cleans Scopes before switching defaults; a default change
does not migrate or unload existing deployments.

V1 relationship: new V2 backend; V1 Python behavior is unchanged. Internal changes:
shared intent, direct Client, selection and recovery. Existing Client ports and
persisted TargetRef shape remain compatible.
