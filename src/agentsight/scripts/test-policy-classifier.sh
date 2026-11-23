#!/usr/bin/env bash
set -euo pipefail

engine="$(cd "$(dirname "${BASH_SOURCE[0]}")/../crates/ebpf-ifc-engine" && pwd)"
python3 - "$engine/process.bpf.c" <<'PY'
import os
import pathlib
import subprocess
import sys
import tempfile

source = pathlib.Path(sys.argv[1]).read_text()


def function(name):
    start = source.rfind("static __always_inline", 0, source.index(name + "("))
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


# Exercise the production relation/classify bodies with deterministic walk outcomes.
harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#undef __always_inline
#define __always_inline
#define TE_TREE_WALK_MAX 256
#define TE_POLICY_CLASS_UNKNOWN -1
#define TE_POLICY_CLASS_OUTSIDE 0
#define TE_POLICY_CLASS_EXCLUDED 1
#define TE_POLICY_CLASS_PROTECTED 2
#define BPF_CORE_READ_INTO(out, ptr, field) ((*(out) = (ptr)->field), 0)
#define BPF_CORE_READ(ptr, field) ((ptr)->field)
typedef uint32_t __u32;
typedef uint64_t __u64;
struct inode { unsigned int i_mode; };
struct dentry { struct inode *d_inode; };
struct vfsmount { int unused; };
struct mount { int unused; };
struct path { struct vfsmount *mnt; struct dentry *dentry; };
struct inode_guard_val { __u32 flags, rule_id, domain_id; };
struct te_path_key { __u32 mnt_id; };
struct te_policy_stable_val { __u64 target_mask; };
struct te_policy_slot_val { __u64 direct_mask, exact_exclude_mask, subtree_exclude_mask; };
struct te_policy_domain_val { __u32 generation; __u64 tree_mask; struct inode_guard_val guard; };
struct te_policy_walk {
    struct dentry *dentry;
    struct mount *mount;
    __u32 domain_id, generation;
    __u64 root_mask, subtree_exclude_mask;
    int relation;
};
struct te_policy_classification {
    struct inode_guard_val guard;
    __u64 protected_mask, excluded_mask;
    int kind;
    bool stable;
};
static int te_policy_stable;
static const struct te_policy_stable_val *configured_stable;
static bool underlying_complete, namespace_complete;
static __u64 discovered_roots, exclusions;
static struct mount fixture_mount;
static struct mount *te_real_mount(struct vfsmount *mnt) { (void)mnt; return &fixture_mount; }
static int te_path_identity(struct dentry *d, struct mount *m, __u32 dom,
                           __u32 gen, struct te_path_key *key) {
    (void)d; (void)m; (void)dom; (void)gen; (void)key; return 0;
}
static const struct te_policy_slot_val *te_policy_slot(struct dentry *p, struct mount *m,
        struct dentry *d, __u32 dom, __u32 gen) {
    (void)p; (void)m; (void)d; (void)dom; (void)gen; return 0;
}
static const void *bpf_map_lookup_elem(void *map, void *key) {
    assert(map == &te_policy_stable);
    assert(((struct te_path_key *)key)->mnt_id == 0);
    return configured_stable;
}
static struct inode_guard_val te_policy_guard_for_mask(
        const struct te_policy_domain_val *dom, __u64 mask) { (void)mask; return dom->guard; }
static long te_policy_underlying_step(__u32 index, void *data) {
    (void)index;
    struct te_policy_walk *walk = data;
    walk->root_mask = discovered_roots;
    walk->relation = underlying_complete ? TE_POLICY_CLASS_OUTSIDE : TE_POLICY_CLASS_UNKNOWN;
    return 1;
}
static long te_policy_namespace_step(__u32 index, void *data) {
    (void)index;
    struct te_policy_walk *walk = data;
    walk->subtree_exclude_mask = exclusions;
    walk->relation = namespace_complete ? TE_POLICY_CLASS_OUTSIDE : TE_POLICY_CLASS_UNKNOWN;
    return 1;
}
static long bpf_loop(__u32 budget, long (*step)(__u32, void *), void *data, __u64 flags) {
    assert(budget == 256); (void)flags; return step(0, data);
}
'''
harness += function("te_policy_relation") + "\n" + function("te_policy_classify")
harness += r'''
static void check_classification(__u64 roots, __u64 mask, __u64 excluded,
        bool u, bool n, int expected, unsigned int inode_mode,
        const struct te_policy_stable_val *stable, bool expected_stable) {
    discovered_roots = roots; exclusions = excluded;
    underlying_complete = u; namespace_complete = n;
    configured_stable = stable;
    struct inode inode = { .i_mode = inode_mode };
    struct dentry entry = { .d_inode = &inode };
    struct vfsmount mount = {0};
    struct path path = { .mnt = &mount, .dentry = &entry };
    struct te_policy_domain_val domain = { .generation = 1, .tree_mask = 3 };
    struct te_policy_classification result;
    te_policy_classify(&path, &entry, true, mask, &domain, &result);
    assert(result.kind == expected);
    assert(result.stable == expected_stable);
    if (expected_stable)
        assert(result.protected_mask == (stable->target_mask & mask));
}
static void check(__u64 roots, __u64 mask, __u64 excluded, bool u, bool n, int expected) {
    check_classification(roots, mask, excluded, u, n, expected, 0100000, NULL, false);
}
static void check_stable(__u64 stable_mask, __u64 roots, __u64 excluded,
        bool u, bool n, int expected, bool expected_stable) {
    struct te_policy_stable_val stable = { .target_mask = stable_mask };
    check_classification(roots, 1, excluded, u, n, expected, 0040000,
                         &stable, expected_stable);
}
int main(void) {
    check(2, 1, 0, false, false, TE_POLICY_CLASS_UNKNOWN);
    check(2, 1, 0, true, false, TE_POLICY_CLASS_UNKNOWN);
    check(2, 1, 0, false, true, TE_POLICY_CLASS_UNKNOWN);
    check(2, 1, 0, true, true, TE_POLICY_CLASS_OUTSIDE);
    check(1, 1, 0, false, false, TE_POLICY_CLASS_PROTECTED);
    check(1, 1, 1, true, true, TE_POLICY_CLASS_EXCLUDED);
    check(1, 1, 1, false, true, TE_POLICY_CLASS_UNKNOWN);
    check(0, 1, 0, true, true, TE_POLICY_CLASS_OUTSIDE);

    /* A stable entry for another operation must not make a directory stable. */
    check_stable(2, 2, 0, true, true, TE_POLICY_CLASS_OUTSIDE, false);
    check_stable(2, 0, 0, true, true, TE_POLICY_CLASS_OUTSIDE, false);
    check_stable(2, 1, 1, true, true, TE_POLICY_CLASS_EXCLUDED, false);
    check_stable(2, 2, 0, false, false, TE_POLICY_CLASS_UNKNOWN, false);
    check_stable(2, 2, 0, true, false, TE_POLICY_CLASS_UNKNOWN, false);
    check_stable(2, 2, 0, false, true, TE_POLICY_CLASS_UNKNOWN, false);
    /* Relevant stable bits retain direct protection despite incomplete walks. */
    check_stable(1, 0, 1, false, false, TE_POLICY_CLASS_PROTECTED, true);
    check_stable(3, 0, 1, false, false, TE_POLICY_CLASS_PROTECTED, true);
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix="fp-classifier-") as tmp:
    c = pathlib.Path(tmp) / "classifier.c"
    exe = pathlib.Path(tmp) / "classifier"
    c.write_text(harness)
    subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra",
                    "-Werror", str(c), "-o", str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print("Production classifier source regressions passed (8 relation + 8 stable-mask outcomes).")
PY
