# `skillfs-fuse` crate layout

The FUSE crate used to live in a single ~7800-line `lib.rs`. It is now split
into focused modules. Behavior, public API, and POSIX semantics are unchanged
relative to the pre-refactor file — every split is mechanical.

## File map

```
crates/skillfs-fuse/src/
├── lib.rs                  Public surface: re-exports, FuseError, MountOptions, MountHandle, inline tests
├── mount.rs                mount_inner + the 10 public mount* / mount_background* entry points
│
├── path.rs                 PathType + parse_path / parse_inbox_components / find_common_path_prefix / is_skill_discover_path
├── inode.rs                InodeManager + InodeEntry
├── handles.rs              HandleEntry / DirHandleEntry / HandleManager + open_options_from_flags
├── sync.rs                 SyncEvent + spawn_sync_worker
│
├── attr.rs                 file_attr_from_stat / file_attr_from_metadata / dir_entry_file_type / filetype_from_mode / system_time_from_secs
├── sys.rs                  errno + rename_noreplace + *at-family helpers (open_dir_path, openat_leaf, mkdirat_leaf, …)
├── xattr.rs                XattrNamespace + namespace classifier + xattr_l{get,list,set,remove} libc wrappers + user-namespace filter
├── symlink_policy.rs       SymlinkTargetClass + classify_symlink_target + resolve_same_skill_relative + symlink_class_label
│
├── security.rs             (pre-existing) Skill Security extension seam (re-exports submodules)
├── security/               (pre-existing) policy, event, audit, drift, lifecycle, inbox, ledger, etc.
│
├── fs.rs                   SkillFs definition, constructor + with_* builders, virtual_file_attr / dir_attr / ro_warn, and the thin trait impl block
└── fs/                     Helpers and callback implementations
    ├── discover.rs         get_skill_discover_content / simple_discover_md
    ├── events.rs           emit_event / emit_op_event / emit_xattr_event / demo_observe / inbox_observe_install_complete / send_sync
    ├── paths.rs            source_base / skill_inode_path / skills_dir_ino / skill_physical_dir / inbox_skill_dir / is_inbox_skill_name_allowed / skill_source_path / primary_skill_names / skill_physical_path / build_fuse_path / resolve_physical_path / open_parent_dir_for
    ├── policy.rs           evaluate_trusted_writer / policy_check / enforce_skill_meta / lifecycle_reservation / enforce_lifecycle_reservation / check_physical_access_result
    ├── read_resolution.rs  ReadResolution enum + resolve_skill_read / snapshot_read_dir / compiled_skill_md / skill_read_dir
    ├── transform_cache.rs  Bounded reuse of exact transformed results
    └── callbacks/          FUSE callback bodies, one file per semantic group
        ├── mod.rs          Submodule declarations
        ├── meta.rs         lookup, getattr, access, statfs
        ├── read.rs         open, read, release, flush, fsync
        ├── dir.rs          opendir, readdir, releasedir, fsyncdir, plus the readdir_dynamic helper
        ├── write.rs        write, create, mknod, setattr
        ├── mutate.rs       mkdir, unlink, rmdir, rename
        ├── link.rs         readlink, symlink, link
        └── xattr.rs        getxattr, listxattr, setxattr, removexattr
```

## Design decisions

### Single `impl Filesystem for SkillFs` block

Rust does not allow splitting a trait impl across multiple files. To get
physical-file modularity for the 26 FUSE callbacks while keeping the trait
impl in one place, `fs.rs` holds the trait impl and each callback is
a one-line wrapper:

```rust
fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
    self.lookup_impl(_req, parent, name, reply)
}
```

The actual callback body lives in `fs/callbacks/<group>.rs` as
`pub(in crate::fs) fn lookup_impl(...)`. Calling
`self.<callback>_impl(...)` resolves to that inherent method.

### Visibility scope: `pub(super)` vs `pub(in crate::fs)`

* Helpers in `fs/discover.rs`, `fs/events.rs`, `fs/paths.rs`,
  `fs/policy.rs`, `fs/read_resolution.rs` are `pub(super)` — visible to
  their parent (`fs.rs`) and, by inclusion, to every other module
  under `fs/`.
* Callback impls in `fs/callbacks/<group>.rs` use `pub(in crate::fs)`,
  one level wider than `pub(super)`. `pub(super)` from
  `crate::fs::callbacks::meta` would only reach `crate::fs::callbacks`,
  which is *not* where the trait impl block in `fs.rs` lives.
  `pub(in crate::fs)` makes the method visible to the entire `fs/`
  subtree, including the wrapper in `fs.rs`.

Both are tighter than `pub(crate)` — none of these helpers leak to
`lib.rs` or external consumers.

### Multiple `impl SkillFs { ... }` blocks

Rust allows multiple inherent impl blocks for the same type across
files in the same crate. Each helper file declares its own
`impl SkillFs { ... }` block; the methods accumulate on the `SkillFs`
type as if they had all been written in one place.

### Dependency direction

The split is acyclic. Roughly:

```
lib.rs → mount.rs → fs::SkillFs
lib.rs → fs::SkillFs

fs.rs → fs/{discover, events, paths, policy, read_resolution, transform_cache, callbacks}
fs/callbacks/* → fs/{paths, events, policy, read_resolution, discover}
fs/policy.rs   → fs/events.rs        (uses emit_event)
fs/read_resolution.rs → fs/paths.rs  (uses source_base / skill_physical_dir / etc.)
fs/read_resolution.rs → fs/discover.rs (compiled_skill_md special-cases skill-discover)

(any of the above) → crate::{attr, handles, inode, path, sync, sys, xattr, security, symlink_policy}
```

No module imports its parent or a sibling that imports it.

## Where to make changes

| Want to change | Edit |
|---|---|
| FUSE callback behavior (e.g. `read` semantics) | `fs/callbacks/<group>.rs` — find `<name>_impl` |
| Path-type classification or virtual layout | `path.rs` |
| Audit event shape or sink dispatch | `fs/events.rs` (+ `security/event.rs`, `security/audit.rs`) |
| `.skill-meta/**` or lifecycle gate | `fs/policy.rs` (+ `security/policy.rs`) |
| Ledger read decision (snapshot vs source vs hidden) | `fs/read_resolution.rs` |
| `skill-discover` content | `fs/discover.rs` |
| Inode bookkeeping | `inode.rs` |
| File-handle bookkeeping | `handles.rs` |
| `*at`-family fallback for long paths | `sys.rs` |
| `FileAttr` conversion | `attr.rs` |
| Xattr passthrough | `xattr.rs` |
| Symlink classifier (no syscalls) | `symlink_policy.rs` |
| Mount entry-point signatures | `mount.rs` (and re-export list in `lib.rs`) |
| Public API surface | `lib.rs` (`pub use` lines) |
