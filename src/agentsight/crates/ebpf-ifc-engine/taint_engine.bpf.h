/* SPDX-License-Identifier: GPL-2.0 OR BSD-3-Clause */
/* Copyright (c) 2026 eunomia-bpf org. */
#ifndef __TAINT_ENGINE_BPF_H
#define __TAINT_ENGINE_BPF_H

/*
 * ActPlane in-kernel taint engine. Owns the label state (process / file /
 * endpoint) + lineage/session gates + the compiled rule tables (writable
 * array maps extended by admitted runtime deltas), and provides the te_* helpers
 * a hook program calls.
 * Requires vmlinux.h + bpf_helpers.h + "taint.h" already included.
 */

#ifndef __noinline
#define __noinline __attribute__((noinline))
#endif

/* ── Writable policy tables (defined before capability.bpf.h so the delta
 *    handler in the drain callback can reference them). ──────────────── */

/* Compiled kernel IR tables. Stored in writable array maps so userspace can
 * append admitted runtime deltas through the cap_req ring buffer. Loop counts
 * live in ts_counts (slots 0=rules, 1=updates) and drive bpf_loop iteration. */
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, MAX_TAINT_UPDATES);
	__type(key, __u32);
	__type(value, struct taint_update);
} ts_updates SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, MAX_TAINT_RULES);
	__type(key, __u32);
	__type(value, struct taint_rule);
} ts_rules SEC(".maps");

/* Loop trip counts in a (non-frozen) map so the verifier treats them as unknown
 * scalars: every table loop runs via bpf_loop(), whose callback is then verified
 * exactly ONCE (a frozen/known count would make the verifier simulate per
 * iteration and -E2BIG at scale). Slots: 0=rules 1=updates 5=labels. */
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 6);
	__type(key, __u32);
	__type(value, __u32);
} ts_counts SEC(".maps");

#include "capability.bpf.h"

/* BPF implementations of the path/pattern matchers. These run inner byte scans
 * through bpf_loop so rule/update callbacks do not multiply verifier state by
 * TAINT_PAT_LEN on kernels with a tighter complexity budget. */
#ifdef __BPF__
struct __taint_cmp_ctx {
	const char *a;
	const char *b;
	long diff;
	long anynz;
};

static long __taint_streq_cb(__u32 idx, void *_ctx)
{
	struct __taint_cmp_ctx *c = _ctx;
	unsigned char ac = 0, bc = 0;

	TE_COPY(&ac, 1, c->a + idx);
	TE_COPY(&bc, 1, c->b + idx);
	c->diff |= (unsigned char)(ac ^ bc);
	return 0;
}

static __noinline int taint_streq(const char *a, const char *b)
{
	struct __taint_cmp_ctx c = { .a = a, .b = b };

	bpf_loop(TAINT_PAT_LEN, __taint_streq_cb, &c, 0);
	return c.diff == 0;
}

static long __taint_prefix_cb(__u32 idx, void *_ctx)
{
	struct __taint_cmp_ctx *c = _ctx;
	unsigned char tc = 0, pc = 0;
	long m;

	TE_COPY(&tc, 1, c->a + idx);
	TE_COPY(&pc, 1, c->b + idx);
	m = te_nzmask(pc);
	c->anynz |= m;
	c->diff |= m & (unsigned char)(tc ^ pc);
	return 0;
}

static __noinline int taint_prefix(const char *text, const char *pre)
{
	struct __taint_cmp_ctx c = { .a = text, .b = pre };

	bpf_loop(TAINT_PAT_LEN, __taint_prefix_cb, &c, 0);
	return c.anynz != 0 && c.diff == 0;
}

/* Substring search via bpf_loop. The outer scan runs through bpf_loop so the
 * callback is verified once. */
struct __taint_contains_ctx {
	const char *text;
	const char *pat;
	int pn;
	int max_pos;
	int found;
};

static long __taint_contains_cb(__u32 idx, void *_ctx)
{
	struct __taint_contains_ctx *c = _ctx;
	if (c->found)
		return 1;
	if (idx > (unsigned int)c->max_pos)
		return 1;

	char window[TAINT_SUF_MAX] = {};
	TE_COPY(window, TAINT_SUF_MAX, c->text + idx);

	long diff = 0;
	TAINT_UNROLL
	for (int j = 0; j < TAINT_SUF_MAX; j++) {
		unsigned char pc = 0;
		TE_COPY(&pc, 1, c->pat + j);
		long jm = -(long)(j < c->pn);
		diff |= jm & (unsigned char)(window[j] ^ pc);
	}
	if (diff == 0)
		c->found = 1;
	return 0;
}

static __noinline int taint_contains(const char *text, const char *pat)
{
	int tn = 0, pn = 0;
	long tlive = 1, plive = 1;

	TAINT_UNROLL
	for (int i = 0; i < TAINT_PAT_LEN; i++) {
		tlive &= te_nzmask((unsigned char)text[i]) & 1;
		tn += (int)tlive;
		plive &= te_nzmask((unsigned char)pat[i]) & 1;
		pn += (int)plive;
	}
	if (pn == 0 || pn > tn || pn > TAINT_SUF_MAX)
		return 0;

	struct __taint_contains_ctx c = {
		.text = text, .pat = pat,
		.pn = pn, .max_pos = tn - pn, .found = 0
	};
	bpf_loop(TAINT_PAT_LEN, __taint_contains_cb, &c, 0);
	return c.found;
}
#endif /* __BPF__ */

static __always_inline int te_path_match(unsigned int kind, const char *text,
					 const char *pat)
{
	switch (kind) {
	case TAINT_MATCH_PREFIX:
		return taint_prefix(text, pat);
	case TAINT_MATCH_SUFFIX:
		if (!(policy_features & TE_POLICY_PATH_SUFFIX))
			return 0;
		return taint_suffix(text, pat);
	case TAINT_MATCH_ANY:
		return 1;
	case TAINT_MATCH_CONTAINS:
		if (!(policy_features & TE_POLICY_PATH_CONTAINS))
			return 0;
		return taint_contains(text, pat);
	default:
		return taint_streq(text, pat);
	}
}

struct proc_state {
	__u64 labels;
	__u64 lin_gates; /* gate bits seen in this pid's ancestor chain (incl self) */
};

struct pid_domain_id {
	pid_t pid;
	__u32 domain_id;
};

/* One causal origin for one label bit on one subject/object. This is intentionally
 * compact: the enforcer keeps the source that introduced the label, then feedback
 * combines it with the violating operation to describe the causal path. */
struct te_prov {
	__u64 label;
	__u64 timestamp_ns;
	pid_t pid;
	__u32 op;
	__u32 ip;
	char target[MAX_FILENAME_LEN];
};

struct pid_label_id {
	pid_t pid;
	__u32 domain_id;
	__u64 label;
};

/* Per-session (keyed by root pid) gate + staleness state.
 *  - gate_bits: v1 latching "after exec X" bits (also used by `since_mask==0`).
 *  - epoch:     monotonic per-session event counter.
 *  - gate_epoch[i]:  epoch of the most recent exec matching gate i (0 = never).
 *  - inval_epoch[i]: epoch of the most recent event matching invalidator i.
 * `after X since Y` is satisfied iff gate_epoch[X] > max(inval_epoch[Y in mask]).
 * It is a HASH map value, mutated in place via the lookup pointer. */
struct te_sess {
	__u64 gate_bits;
	__u32 epoch;
	__u32 _pad;
	__u32 gate_epoch[MAX_TAINT_GATES];
	__u32 inval_epoch[MAX_TAINT_INVALS];
};
struct sess_domain_id {
	pid_t root;
	__u32 domain_id;
};

/* File object identity (Layer A, docs/rule-language.md §1.10). A real
 * (dev, inode) when the hook or a post-syscall fdtable lookup can reach the
 * file object. Otherwise (0, fnv1a(path)) preserves tracepoint path-keyed
 * fallback behavior. Used as a HASH key, so it MUST be fully zero-initialized
 * (incl _pad) before use. */
struct file_id {
	__u64 ino;
	__u32 dev;
	__u32 _pad;
};
struct file_domain_id {
	struct file_id fid;
	__u32 domain_id;
	__u32 _pad;
};
struct file_label_id {
	struct file_domain_id fdom;
	__u64 label;
};
/* ts_file value: taint labels (as before) plus the epoch of this object's last
 * observed write. last_write_epoch is infrastructure for staleness precision
 * (Layer B); Layer A only populates it for files that already carry labels. */
struct file_state {
	__u64 labels;
	__u32 last_write_epoch;
	__u32 _pad;
};

struct endp_domain_id {
	__u32 ip;
	__u32 domain_id;
};
struct endp_label_id {
	struct endp_domain_id edom;
	__u64 label;
};

static __always_inline void te_file_domain_key_for(__u32 domain_id,
						   struct file_id *fid,
						   struct file_domain_id *out)
{
	out->fid = *fid;
	out->domain_id = domain_id;
	out->_pad = 0;
}

static __always_inline void te_endp_domain_key(pid_t pid, __u32 ip,
					       struct endp_domain_id *out)
{
	out->ip = ip;
	out->domain_id = cap_domain_for_pid(pid);
}

static __always_inline void te_endp_domain_key_for(__u32 domain_id, __u32 ip,
						   struct endp_domain_id *out)
{
	out->ip = ip;
	out->domain_id = domain_id;
}

struct te_rule_eval {
	pid_t pid;
	__u64 global_labels;
	__u64 current_labels;
	unsigned int op;
	char target[MAX_FILENAME_LEN];
	struct file_id fid;
	__u32 has_fid;
	__u32 ip;
	__u32 current_domain_id;
	__u32 include_file_labels;
	__u32 matched_domain_id;
	unsigned int effect;
	unsigned int effect_mask;
	int matched_rule;       /* set by te_rule_effect on a hit */
	int matched_index;      /* table index used for same-effect tie-breaking */
	__u64 matched_req;      /* required label mask for the matched compiled rule */
	__u64 matched_labels;   /* domain-specific labels used by the matched rule */
};

struct eval_scratch {
	struct te_rule_eval eval;
};
struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct eval_scratch);
} ts_eval_scratch SEC(".maps");

static __always_inline struct eval_scratch *eval_scratch_buf(void)
{
	__u32 key = 0;

	return bpf_map_lookup_elem(&ts_eval_scratch, &key);
}

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, pid_t);
	__type(value, struct proc_state);
} ts_proc SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct pid_domain_id);
	__type(value, struct proc_state);
} ts_proc_domains SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct pid_label_id);
	__type(value, struct te_prov);
} ts_proc_prov SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, pid_t);
	__type(value, pid_t);
} ts_root SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, struct sess_domain_id);
	__type(value, struct te_sess);
} ts_sess SEC(".maps"); /* root pid -> session gate + staleness state */

/* A single never-written, zero-initialized te_sess used only as the seed value
 * for new ts_sess entries (te_sess is too big to zero on the BPF stack). */
struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct te_sess);
} ts_sess_zero SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct file_domain_id);
	__type(value, struct file_state);
} ts_file SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct file_label_id);
	__type(value, struct te_prov);
} ts_file_prov SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, struct endp_domain_id); /* IPv4 (network order) + domain */
	__type(value, __u64);
} ts_endp SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct endp_label_id);
	__type(value, struct te_prov);
} ts_endp_prov SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct file_domain_id);
} ts_fdom_tmp SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct te_prov);
} ts_prov_tmp SEC(".maps");

static __always_inline struct file_domain_id *te_file_domain_tmp(void)
{
	__u32 k = 0;
	return bpf_map_lookup_elem(&ts_fdom_tmp, &k);
}

static __always_inline unsigned int te_count(__u32 slot)
{
	__u32 *v = bpf_map_lookup_elem(&ts_counts, &slot);
	return v ? *v : 0;
}

/* Per-CPU scratch for the raw argv blob + tokenized slots (off-stack; the exec
 * hook fills it then matches synchronously, so per-CPU reuse is safe). Both live
 * in the map so the bpf_loop tokenizer can do variable-offset reads/writes, which
 * the verifier rejects on the stack. */
struct te_argslots {
	char blob[TAINT_ARGV_CAP];
	char slots[TAINT_ARG_SLOTS_BUF];
};
struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct te_argslots);
} ts_argslots SEC(".maps");

struct te_exit_gate_pending {
	__u64 gates;
	__u32 epoch;
	__u32 _pad;
};

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, struct pid_domain_id);
	__type(value, struct te_exit_gate_pending);
} ts_exit_gates SEC(".maps");

static __always_inline struct te_argslots *te_argslots_buf(void)
{
	__u32 k = 0;
	return bpf_map_lookup_elem(&ts_argslots, &k);
}

/* Tokenize the argv blob into fixed slots via bpf_loop (callback verified once;
 * a plain inlined loop's dynamic-index write exploded the verifier). The callback
 * re-looks-up the per-CPU scratch so the verifier keeps the map_value bound (a
 * pointer carried through the bpf_loop ctx loses it -> "unbounded access"). */
struct te_tok_ctx { int si; int pj; };
static int te_tok_cb(__u32 i, void *vc)
{
	struct te_tok_ctx *c = vc;
	struct te_argslots *a = te_argslots_buf();

	if (!a)
		return 1;
	/* barrier_var keeps clang from eliding the mask (it otherwise proves i<CAP and
	 * reuses an unbounded copy for the access); the AND makes bi<=CAP-1 verifiably. */
	__u32 bi = i;
	barrier_var(bi);
	bi &= (__u32)(TAINT_ARGV_CAP - 1);
	char ch = a->blob[bi];
	if (ch == '\0') {
		c->si++;
		c->pj = 0;
		return c->si >= MAX_ARG_SLOTS ? 1 : 0;
	}
	if (c->si >= 0 && c->si < MAX_ARG_SLOTS && c->pj >= 0 && c->pj < TAINT_ARG_LEN - 1) {
		__u32 sidx = ((__u32)c->si * TAINT_ARG_LEN + (__u32)c->pj);
		if (sidx < TAINT_ARG_SLOTS_BUF)
			a->slots[sidx] = ch;
		c->pj++;
	}
	return 0;
}
static __always_inline void te_tokenize_args_eng(int len)
{
	if (len < 0)
		len = 0;
	if (len > TAINT_ARGV_CAP)        /* so the callback's i < len stays bounded */
		len = TAINT_ARGV_CAP;
	struct te_tok_ctx c = { .si = 0, .pj = 0 };
	bpf_loop((unsigned int)len, te_tok_cb, &c, 0);
}

#ifdef __BPF__
/* BPF-side argv token matcher. The generic C matcher in taint.h is intentionally
 * not used here: comparing every slot inline inside the rule/update callbacks
 * pushes the exec-argv hook over the verifier instruction limit. This version
 * verifies one slot-compare callback and re-looks-up the per-CPU argv scratch so
 * the verifier keeps a bounded map_value for the variable slot offset. */
struct __taint_arg_match_ctx {
	char tok[TAINT_ARG_LEN];
	int found;
};

static long __taint_arg_match_cb(__u32 idx, void *_ctx)
{
	struct __taint_arg_match_ctx *c = _ctx;
	struct te_argslots *a = te_argslots_buf();

	if (c->found)
		return 1;
	if (!a)
		return 1;
	if (idx >= MAX_ARG_SLOTS)
		return 1;

	__u32 off = idx * TAINT_ARG_LEN;
	if (off >= TAINT_ARG_SLOTS_BUF)
		return 1;

	char slot[TAINT_ARG_LEN] = {};
	TE_COPY(slot, TAINT_ARG_LEN, a->slots + off);

	long diff = 0;
	TAINT_UNROLL
	for (int j = 0; j < TAINT_ARG_LEN; j++)
		diff |= (unsigned char)(slot[j] ^ c->tok[j]);
	if (diff == 0)
		c->found = 1;
	return 0;
}

static __noinline int taint_arg_match(const char *slots, const char *tok)
{
	(void)slots;
	struct __taint_arg_match_ctx c = {};

	TAINT_UNROLL
	for (int j = 0; j < TAINT_ARG_LEN; j++)
		c.tok[j] = tok[j];
	if (c.tok[0] == '\0')
		return 1;
	bpf_loop(MAX_ARG_SLOTS, __taint_arg_match_cb, &c, 0);
	return c.found;
}
#endif /* __BPF__ */

static __always_inline __u64 te_fnv1a(const char *s)
{
	__u64 h = 0xcbf29ce484222325ULL;
	TAINT_UNROLL
	for (int i = 0; i < TAINT_PAT_LEN; i++) {
		unsigned char c = (unsigned char)s[i];
		h ^= c;
		h *= 0x100000001b3ULL;
	}
	return h;
}

static __always_inline struct proc_state *te_get(pid_t pid)
{
	return bpf_map_lookup_elem(&ts_proc, &pid);
}

static __always_inline void te_pid_domain_key(pid_t pid, __u32 domain_id,
					      struct pid_domain_id *out)
{
	out->pid = pid;
	out->domain_id = domain_id;
}

static __always_inline struct proc_state *te_get_domain(pid_t pid, __u32 domain_id)
{
	if (domain_id == 0)
		return te_get(pid);
	struct pid_domain_id key = {};
	te_pid_domain_key(pid, domain_id, &key);
	return bpf_map_lookup_elem(&ts_proc_domains, &key);
}

static __always_inline int te_pid_active(pid_t pid)
{
	return cap_task_bound(pid);
}

static __always_inline __u32 te_domain_for_depth(pid_t pid, int depth)
{
	if (depth == 0)
		return 0;
	__u32 current = cap_domain_for_pid(pid);
	if (!current)
		return 0;
	if (depth == 1)
		return current;
	for (int i = 2; i < CAP_DOMAIN_DEPTH; i++) {
		current = cap_parent_domain(current);
		if (!current)
			return 0;
		if (i == depth)
			return current;
	}
	return 0;
}

static __always_inline __u64 te_labels_for_domain(pid_t pid, __u32 domain_id)
{
	if (!te_pid_active(pid))
		return 0;
	if (!cap_domain_matches_pid(pid, domain_id))
		return 0;
	struct proc_state *p = te_get_domain(pid, domain_id);
	__u64 labels = p ? p->labels : 0;
	return labels | cap_labels_for_pid_domain(pid, domain_id);
}

static __always_inline __u64 te_labels(pid_t pid)
{
	return te_labels_for_domain(pid, cap_domain_for_pid(pid));
}

static __always_inline void te_copy_target(char *dst, const char *src)
{
	if (!src) {
		dst[0] = '\0';
		return;
	}
	for (int i = 0; i < MAX_FILENAME_LEN; i++)
		dst[i] = src[i];
	dst[MAX_FILENAME_LEN - 1] = '\0';
}

static __always_inline struct te_prov *te_prov_tmp(void)
{
	__u32 k = 0;
	return bpf_map_lookup_elem(&ts_prov_tmp, &k);
}

static __always_inline struct te_prov *te_lookup_proc_prov(pid_t pid, __u32 domain_id,
							   __u64 label)
{
	struct pid_label_id key = { .pid = pid, .domain_id = domain_id, .label = label };
	return bpf_map_lookup_elem(&ts_proc_prov, &key);
}

static __always_inline void te_record_proc_prov(pid_t pid, __u32 domain_id,
						__u64 label,
						unsigned int op, const char *target,
						__u32 ip)
{
	if (!label)
		return;
	struct te_prov *p = te_prov_tmp();
	if (!p)
		return;
	__builtin_memset(p, 0, sizeof(*p));
	p->label = label;
	p->timestamp_ns = bpf_ktime_get_ns();
	p->pid = pid;
	p->op = op;
	p->ip = ip;
	te_copy_target(p->target, target);
	struct pid_label_id key = { .pid = pid, .domain_id = domain_id, .label = label };
	bpf_map_update_elem(&ts_proc_prov, &key, p, BPF_ANY);
}

struct te_record_proc_prov_ctx {
	pid_t pid;
	__u32 domain_id;
	__u64 labels;
	unsigned int op;
	const char *target;
	__u32 ip;
};
static int te_record_proc_prov_cb(__u32 i, void *vc)
{
	struct te_record_proc_prov_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (c->labels & bit)
		te_record_proc_prov(c->pid, c->domain_id, bit, c->op, c->target, c->ip);
	return 0;
}
static __always_inline void te_record_proc_prov_mask(pid_t pid, __u32 domain_id,
						     __u64 labels,
						     unsigned int op,
						     const char *target, __u32 ip)
{
	struct te_record_proc_prov_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.labels = labels,
		.op = op,
		.target = target,
		.ip = ip,
	};
	bpf_loop(te_count(5), te_record_proc_prov_cb, &c, 0);
}

struct te_copy_proc_prov_ctx { pid_t from, to; __u32 domain_id; __u64 labels; };
static int te_copy_proc_prov_cb(__u32 i, void *vc)
{
	struct te_copy_proc_prov_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct te_prov *p = te_lookup_proc_prov(c->from, c->domain_id, bit);
	if (!p)
		return 0;
	struct pid_label_id key = { .pid = c->to, .domain_id = c->domain_id, .label = bit };
	bpf_map_update_elem(&ts_proc_prov, &key, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_proc_prov(pid_t from, pid_t to, __u32 domain_id,
					 __u64 labels)
{
	struct te_copy_proc_prov_ctx c = {
		.from = from, .to = to, .domain_id = domain_id, .labels = labels
	};
	bpf_loop(te_count(5), te_copy_proc_prov_cb, &c, 0);
}

struct te_copy_file_to_file_ctx {
	struct file_domain_id from;
	struct file_domain_id to;
	__u64 labels;
};
static int te_copy_file_prov_to_file_cb(__u32 i, void *vc)
{
	struct te_copy_file_to_file_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct file_label_id from = { .fdom = c->from, .label = bit };
	struct te_prov *p = bpf_map_lookup_elem(&ts_file_prov, &from);
	if (!p)
		return 0;
	struct file_label_id to = { .fdom = c->to, .label = bit };
	bpf_map_update_elem(&ts_file_prov, &to, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_file_prov_to_file(struct file_domain_id *from,
						 struct file_domain_id *to,
						 __u64 labels)
{
	struct te_copy_file_to_file_ctx c = {
		.from = *from,
		.to = *to,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_copy_file_prov_to_file_cb, &c, 0);
}

struct te_delete_file_prov_ctx {
	struct file_domain_id fdom;
	__u64 labels;
};
static int te_delete_file_prov_cb(__u32 i, void *vc)
{
	struct te_delete_file_prov_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct file_label_id fk = { .fdom = c->fdom, .label = bit };
	bpf_map_delete_elem(&ts_file_prov, &fk);
	return 0;
}
static __noinline void te_delete_file_prov_mask(struct file_domain_id *fdom,
						__u64 labels)
{
	struct te_delete_file_prov_ctx c = {
		.fdom = *fdom,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_delete_file_prov_cb, &c, 0);
}

struct te_copy_file_to_proc_ctx {
	pid_t pid;
	struct file_domain_id fdom;
	__u64 labels;
};
static int te_copy_file_prov_to_proc_cb(__u32 i, void *vc)
{
	struct te_copy_file_to_proc_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct file_label_id fk = { .fdom = c->fdom, .label = bit };
	struct te_prov *p = bpf_map_lookup_elem(&ts_file_prov, &fk);
	if (!p)
		return 0;
	struct pid_label_id pk = {
		.pid = c->pid,
		.domain_id = c->fdom.domain_id,
		.label = bit,
	};
	bpf_map_update_elem(&ts_proc_prov, &pk, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_file_prov_to_proc(pid_t pid, struct file_domain_id *fdom,
						 __u64 labels)
{
	struct te_copy_file_to_proc_ctx c = {
		.pid = pid,
		.fdom = *fdom,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_copy_file_prov_to_proc_cb, &c, 0);
}

struct te_copy_proc_to_file_ctx {
	pid_t pid;
	struct file_domain_id fdom;
	__u64 labels;
};
static int te_copy_proc_prov_to_file_cb(__u32 i, void *vc)
{
	struct te_copy_proc_to_file_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct te_prov *p = te_lookup_proc_prov(c->pid, c->fdom.domain_id, bit);
	if (!p)
		return 0;
	struct file_label_id fk = { .fdom = c->fdom, .label = bit };
	bpf_map_update_elem(&ts_file_prov, &fk, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_proc_prov_to_file(pid_t pid, struct file_domain_id *fdom,
						 __u64 labels)
{
	struct te_copy_proc_to_file_ctx c = {
		.pid = pid,
		.fdom = *fdom,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_copy_proc_prov_to_file_cb, &c, 0);
}

struct te_copy_endp_to_proc_ctx {
	pid_t pid;
	struct endp_domain_id edom;
	__u64 labels;
};
static int te_copy_endp_prov_to_proc_cb(__u32 i, void *vc)
{
	struct te_copy_endp_to_proc_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct endp_label_id ek = { .edom = c->edom, .label = bit };
	struct te_prov *p = bpf_map_lookup_elem(&ts_endp_prov, &ek);
	if (!p)
		return 0;
	struct pid_label_id pk = {
		.pid = c->pid,
		.domain_id = c->edom.domain_id,
		.label = bit,
	};
	bpf_map_update_elem(&ts_proc_prov, &pk, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_endp_prov_to_proc(pid_t pid,
						 struct endp_domain_id *edom,
						 __u64 labels)
{
	struct te_copy_endp_to_proc_ctx c = {
		.pid = pid,
		.edom = *edom,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_copy_endp_prov_to_proc_cb, &c, 0);
}

struct te_copy_proc_to_endp_ctx {
	pid_t pid;
	struct endp_domain_id edom;
	__u64 labels;
};
static int te_copy_proc_prov_to_endp_cb(__u32 i, void *vc)
{
	struct te_copy_proc_to_endp_ctx *c = vc;
	if (i >= MAX_TAINT_LABELS)
		return 1;
	__u64 bit = 1ULL << i;
	if (!(c->labels & bit))
		return 0;
	struct te_prov *p = te_lookup_proc_prov(c->pid, c->edom.domain_id, bit);
	if (!p)
		return 0;
	struct endp_label_id ek = { .edom = c->edom, .label = bit };
	bpf_map_update_elem(&ts_endp_prov, &ek, p, BPF_ANY);
	return 0;
}
static __noinline void te_copy_proc_prov_to_endp(pid_t pid,
						 struct endp_domain_id *edom,
						 __u64 labels)
{
	struct te_copy_proc_to_endp_ctx c = {
		.pid = pid,
		.edom = *edom,
		.labels = labels,
	};
	bpf_loop(te_count(5), te_copy_proc_prov_to_endp_cb, &c, 0);
}

static __always_inline void te_add_labels_domain(pid_t pid, __u32 domain_id, __u64 add)
{
	if (!te_pid_active(pid) || !add)
		return;
	if (!cap_domain_matches_pid(pid, domain_id))
		return;
	struct proc_state *p = te_get_domain(pid, domain_id);
	if (p) {
		p->labels |= add;
	} else {
		struct proc_state ns = { .labels = add, .lin_gates = 0 };
		if (domain_id == 0) {
			bpf_map_update_elem(&ts_proc, &pid, &ns, BPF_ANY);
		} else {
			struct pid_domain_id key = {};
			te_pid_domain_key(pid, domain_id, &key);
			bpf_map_update_elem(&ts_proc_domains, &key, &ns, BPF_ANY);
		}
	}
}

static __always_inline void te_add_labels(pid_t pid, __u64 add)
{
	te_add_labels_domain(pid, cap_domain_for_pid(pid), add);
}
static __always_inline pid_t te_root(pid_t pid)
{
	pid_t *r = bpf_map_lookup_elem(&ts_root, &pid);
	return r ? *r : pid;
}

static __always_inline void te_sess_domain_key(pid_t root, __u32 domain_id,
					       struct sess_domain_id *out)
{
	out->root = root;
	out->domain_id = domain_id;
}

static __always_inline struct te_sess *te_sess_get(pid_t root, __u32 domain_id)
{
	struct sess_domain_id key = {};
	te_sess_domain_key(root, domain_id, &key);
	return bpf_map_lookup_elem(&ts_sess, &key);
}
static __always_inline struct te_sess *te_sess_init(pid_t root, __u32 domain_id)
{
	struct sess_domain_id key = {};
	te_sess_domain_key(root, domain_id, &key);
	struct te_sess *s = bpf_map_lookup_elem(&ts_sess, &key);
	if (s)
		return s;
	/* te_sess is >512B, so a zeroed stack template blows the BPF stack. Seed
	 * the new entry from a never-written, BSS-zeroed per-CPU template instead. */
	__u32 k = 0;
	struct te_sess *z = bpf_map_lookup_elem(&ts_sess_zero, &k);
	if (!z)
		return 0;
	bpf_map_update_elem(&ts_sess, &key, z, BPF_NOEXIST);
	return bpf_map_lookup_elem(&ts_sess, &key);
}

/* Bump and return this session's monotonic event counter, creating the session
 * entry if needed. Returns 0 if the session map is unavailable. Callers tick
 * once per OS event, then stamp every gate/invalidator/file touched at that
 * same epoch — so a write and its invalidator share one ordering point. */
static __noinline __u32 te_tick(pid_t root, __u32 domain_id)
{
	struct te_sess *s = te_sess_init(root, domain_id);
	if (!s)
		return 0;
	s->epoch += 1;
	return s->epoch;
}

struct te_stamp_ctx {
	pid_t root;
	__u32 domain_id;
	__u32 ep;
	__u64 hits;
};

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, struct te_stamp_ctx);
} ts_stamp_scratch SEC(".maps");

static __always_inline struct te_stamp_ctx *te_stamp_scratch_buf(void)
{
	__u32 key = 0;

	return bpf_map_lookup_elem(&ts_stamp_scratch, &key);
}

static long te_stamp_gate_cb(__u32 i, void *vc)
{
	struct te_stamp_ctx *c = te_stamp_scratch_buf();
	__u32 idx = i;

	(void)vc;
	if (!c)
		return 1;
	barrier_var(idx);
	idx &= (__u32)(MAX_TAINT_GATES - 1);
	if (!(c->hits & (1ULL << idx)))
		return 0;
	struct te_sess *s = te_sess_get(c->root, c->domain_id);
	if (!s)
		return 1;
	s->gate_epoch[idx] = c->ep;
	return 0;
}

static long te_stamp_inval_cb(__u32 i, void *vc)
{
	struct te_stamp_ctx *c = te_stamp_scratch_buf();
	__u32 idx = i;

	(void)vc;
	if (!c)
		return 1;
	barrier_var(idx);
	idx &= (__u32)(MAX_TAINT_INVALS - 1);
	if (!(c->hits & (1ULL << idx)))
		return 0;
	struct te_sess *s = te_sess_get(c->root, c->domain_id);
	if (!s)
		return 1;
	s->inval_epoch[idx] = c->ep;
	return 0;
}

/* Stamp the gates and invalidators that fired in one event at `ep`. gate_hits
 * also latch into gate_bits (v1 `after`). The two scans are constant-bound
 * bpf_loop callbacks so older CI verifiers do not path-expand both 64-entry
 * scans into the open/connect/read hook programs. */
static __noinline void te_stamp(pid_t root, __u32 domain_id, __u32 ep,
				__u64 gate_hits, __u64 inval_hits)
{
	if (!ep || (!gate_hits && !inval_hits))
		return;
	struct te_sess *s = te_sess_get(root, domain_id);
	if (!s)
		return;
	s->gate_bits |= gate_hits;
	__u64 loop_ctx = 0;
	struct te_stamp_ctx *c = te_stamp_scratch_buf();
	if (!c)
		return;
	if (gate_hits) {
		c->root = root;
		c->domain_id = domain_id;
		c->ep = ep;
		c->hits = gate_hits;
		bpf_loop(MAX_TAINT_GATES, te_stamp_gate_cb, &loop_ctx, 0);
	}
	if (inval_hits) {
		c->root = root;
		c->domain_id = domain_id;
		c->ep = ep;
		c->hits = inval_hits;
		bpf_loop(MAX_TAINT_INVALS, te_stamp_inval_cb, &loop_ctx, 0);
	}
}

/* Is a TCOND_AFTER condition satisfied (i.e. the deny is relaxed / allowed)?
 *  - since_mask == 0 : v1 latching — the gate fired at some point.
 *  - since_mask != 0 : v2 staleness — the gate fired AND is still fresh, i.e.
 *    no invalidator in since_mask has fired more recently. */
static __noinline int te_after_satisfied(const struct taint_rule *r, pid_t pid)
{
	pid_t rt = te_root(pid);
	struct te_sess *s = te_sess_get(rt, r->domain_id);

	if (!s)
		return 0;
	if (r->since_mask == 0)
		return (s->gate_bits & r->gate) ? 1 : 0;
	__u32 gidx = r->gate_idx;
	if (gidx >= MAX_TAINT_GATES)
		return 0;
	__u32 ge = s->gate_epoch[gidx];
	if (ge == 0)
		return 0; /* gate never fired -> stale */
	__u32 last_inval = 0;
	for (int i = 0; i < MAX_TAINT_INVALS; i++) {
		if (r->since_mask & (1ULL << i)) {
			__u32 ie = s->inval_epoch[i];
			if (ie > last_inval)
				last_inval = ie;
		}
	}
	return ge > last_inval ? 1 : 0;
}

static __always_inline void te_copy_proc_domain_state(pid_t from, pid_t to,
						      __u32 domain_id)
{
	struct proc_state *pp = te_get_domain(from, domain_id);
	if (!pp)
		return;
	struct proc_state ns = *pp;
	if (domain_id == 0) {
		bpf_map_update_elem(&ts_proc, &to, &ns, BPF_ANY);
	} else {
		struct pid_domain_id key = {};
		te_pid_domain_key(to, domain_id, &key);
		bpf_map_update_elem(&ts_proc_domains, &key, &ns, BPF_ANY);
	}
	if (ns.labels)
		te_copy_proc_prov(from, to, domain_id, ns.labels);
}

/* fork: child inherits labels + lineage gates in every active domain; root carries down. */
static __always_inline void te_fork(pid_t ppid, pid_t cpid)
{
	if (!te_pid_active(ppid))
		return;
	cap_fork(ppid, cpid);
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(ppid, i);
		if (i > 0 && !domain_id)
			break;
		te_copy_proc_domain_state(ppid, cpid, domain_id);
	}
	pid_t r = te_root(ppid);
	bpf_map_update_elem(&ts_root, &cpid, &r, BPF_ANY);
}

/* All policy table scans below run via bpf_loop() (count from ts_counts) so the
 * callback is verified once regardless of table size. The update table is the
 * single low-level IR for sources, xforms, gates, and since invalidators. */
struct te_update_ctx {
	pid_t pid;
	__u32 domain_id;
	unsigned int op;
	const char *target;
	__u32 ip;
	__u64 add;
	__u64 del;
	__u64 gates;
	__u64 exit_gates;
	__u64 invals;
};

static __always_inline int te_exec_simple_match(unsigned int kind,
						const char *text,
						const char *pat)
{
	if (kind == TAINT_MATCH_ANY)
		return 1;
	if (kind != TAINT_MATCH_EXACT)
		return 0;
	return taint_streq(text, pat);
}

static int te_update_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;
	int match = 0;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != c->op)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (u.op == TOP_CONNECT)
		match = ((c->ip & u.ipv4_mask) == u.ipv4);
	else if (u.op == TOP_EXEC)
		match = taint_exec_match(u.match, c->target, u.target);
	else
		match = te_path_match(u.match, c->target, u.target);
	if (!match)
		return 0;
	/* For exec updates with an arg constraint, also check argv tokens. */
	if (u.op == TOP_EXEC && u.arg[0] != '\0') {
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, u.arg))
			return 0;
	}
	c->add |= u.add;
	c->del |= u.del;
	if (u.gate_exit_code == TAINT_GATE_IMMEDIATE)
		c->gates |= u.gates;
	else
		c->exit_gates |= u.gates;
	c->invals |= u.invals;
	return 0;
}

static int te_exec_update_simple_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != TOP_EXEC)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (!te_exec_simple_match(u.match, c->target, u.target))
		return 0;
	if (u.arg[0] != '\0') {
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, u.arg))
			return 0;
	}
	c->add |= u.add;
	c->del |= u.del;
	if (u.gate_exit_code == TAINT_GATE_IMMEDIATE)
		c->gates |= u.gates;
	else
		c->exit_gates |= u.gates;
	c->invals |= u.invals;
	return 0;
}

static int te_exec_update_prefix_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != TOP_EXEC)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (u.match == TAINT_MATCH_EXACT || u.match == TAINT_MATCH_ANY)
		return 0;
	if (!taint_exec_match(u.match, c->target, u.target))
		return 0;
	if (u.arg[0] != '\0') {
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, u.arg))
			return 0;
	}
	c->add |= u.add;
	c->del |= u.del;
	if (u.gate_exit_code == TAINT_GATE_IMMEDIATE)
		c->gates |= u.gates;
	else
		c->exit_gates |= u.gates;
	c->invals |= u.invals;
	return 0;
}

static __always_inline void te_collect_updates(struct te_update_ctx *c)
{
	bpf_loop(te_count(1), te_update_cb, c, 0);
}

static int te_exec_update_no_args_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != TOP_EXEC)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (u.arg[0] != '\0')
		return 0;
	if (!taint_exec_match(u.match, c->target, u.target))
		return 0;
	c->add |= u.add;
	c->del |= u.del;
	if (u.gate_exit_code == TAINT_GATE_IMMEDIATE)
		c->gates |= u.gates;
	else
		c->exit_gates |= u.gates;
	c->invals |= u.invals;
	return 0;
}

static __always_inline void te_collect_exec_updates_no_args(struct te_update_ctx *c)
{
	bpf_loop(te_count(1), te_exec_update_no_args_cb, c, 0);
}

static __always_inline void te_update_accum(struct te_update_ctx *c,
					    const struct taint_update *u)
{
	c->add |= u->add;
	c->del |= u->del;
	if (u->gate_exit_code == TAINT_GATE_IMMEDIATE)
		c->gates |= u->gates;
	else
		c->exit_gates |= u->gates;
	c->invals |= u->invals;
}

static int te_file_update_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != c->op)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (!te_path_match(u.match, c->target, u.target))
		return 0;
	te_update_accum(c, &u);
	return 0;
}

static __always_inline void te_collect_file_updates(struct te_update_ctx *c)
{
	bpf_loop(te_count(1), te_file_update_cb, c, 0);
}

static int te_endpoint_update_cb(__u32 i, void *vc)
{
	struct te_update_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	if (u.op != c->op)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if ((c->ip & u.ipv4_mask) != u.ipv4)
		return 0;
	te_update_accum(c, &u);
	return 0;
}

static __always_inline void te_collect_endpoint_updates(struct te_update_ctx *c)
{
	bpf_loop(te_count(1), te_endpoint_update_cb, c, 0);
}

/* on exec: apply compiled updates for labels, declassification, gates, and
 * exec-side invalidators. All exec patterns are matched on comm. */
static __always_inline void te_store_proc_domain(pid_t pid, __u32 domain_id,
						 struct proc_state *state)
{
	if (domain_id == 0) {
		bpf_map_update_elem(&ts_proc, &pid, state, BPF_ANY);
	} else {
		struct pid_domain_id key = {};
		te_pid_domain_key(pid, domain_id, &key);
		bpf_map_update_elem(&ts_proc_domains, &key, state, BPF_ANY);
	}
}

static __always_inline void te_exec_update_domain(pid_t pid, const char *comm,
						  __u32 domain_id)
{
	struct te_update_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.op = TOP_EXEC,
		.target = comm,
	};

	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	te_collect_updates(&c);

	struct proc_state *p = te_get_domain(pid, domain_id);
	struct proc_state ns = { 0 };
	if (p)
		ns = *p;
	ns.labels = (ns.labels | c.add) & ~c.del;
	ns.lin_gates |= c.gates;
	te_store_proc_domain(pid, domain_id, &ns);
	if (c.add)
		te_record_proc_prov_mask(pid, domain_id, c.add, TOP_EXEC, comm, 0);
	if (c.gates || c.exit_gates || c.invals) {
		pid_t r = te_root(pid);
		__u32 ep = te_tick(r, domain_id);
		if (c.gates || c.invals)
			te_stamp(r, domain_id, ep, c.gates, c.invals);
		if (c.exit_gates) {
			struct te_exit_gate_pending pending = {
				.gates = c.exit_gates,
				.epoch = ep,
			};
			struct pid_domain_id key = {};
			te_pid_domain_key(pid, domain_id, &key);
			bpf_map_update_elem(&ts_exit_gates, &key, &pending, BPF_ANY);
		} else {
			struct pid_domain_id key = {};
			te_pid_domain_key(pid, domain_id, &key);
			bpf_map_delete_elem(&ts_exit_gates, &key);
		}
	} else {
		struct pid_domain_id key = {};
		te_pid_domain_key(pid, domain_id, &key);
		bpf_map_delete_elem(&ts_exit_gates, &key);
	}
}

static __always_inline void te_exec_update_domain_no_args(pid_t pid,
							  const char *comm,
							  __u32 domain_id)
{
	struct te_update_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.op = TOP_EXEC,
		.target = comm,
	};

	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	te_collect_exec_updates_no_args(&c);

	struct proc_state *p = te_get_domain(pid, domain_id);
	struct proc_state ns = { 0 };
	if (p)
		ns = *p;
	ns.labels = (ns.labels | c.add) & ~c.del;
	ns.lin_gates |= c.gates;
	te_store_proc_domain(pid, domain_id, &ns);
	if (c.add)
		te_record_proc_prov_mask(pid, domain_id, c.add, TOP_EXEC, comm, 0);
	if (c.gates || c.exit_gates || c.invals) {
		pid_t r = te_root(pid);
		__u32 ep = te_tick(r, domain_id);
		if (c.gates || c.invals)
			te_stamp(r, domain_id, ep, c.gates, c.invals);
		if (c.exit_gates) {
			struct te_exit_gate_pending pending = {
				.gates = c.exit_gates,
				.epoch = ep,
			};
			struct pid_domain_id key = {};
			te_pid_domain_key(pid, domain_id, &key);
			bpf_map_update_elem(&ts_exit_gates, &key, &pending, BPF_ANY);
		} else {
			struct pid_domain_id key = {};
			te_pid_domain_key(pid, domain_id, &key);
			bpf_map_delete_elem(&ts_exit_gates, &key);
		}
	} else {
		struct pid_domain_id key = {};
		te_pid_domain_key(pid, domain_id, &key);
		bpf_map_delete_elem(&ts_exit_gates, &key);
	}
}

static __always_inline void te_exec_update(pid_t pid, const char *comm)
{
	if (!te_pid_active(pid))
		return;
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_exec_update_domain(pid, comm, domain_id);
	}
}

static __always_inline void te_exec_update_no_args(pid_t pid, const char *comm)
{
	if (!te_pid_active(pid))
		return;
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_exec_update_domain_no_args(pid, comm, domain_id);
	}
}

static __noinline __u64 te_update_add_file_domain(unsigned int op,
						  const char *target,
						  pid_t pid, __u32 domain_id)
{
	struct te_update_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.op = op,
		.target = target,
	};

	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return 0;
	te_collect_file_updates(&c);
	return c.add;
}

static __noinline __u64 te_update_add_endpoint_domain(unsigned int op, __u32 ip,
						      pid_t pid,
						      __u32 domain_id)
{
	struct te_update_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.op = op,
		.ip = ip,
	};

	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return 0;
	te_collect_endpoint_updates(&c);
	return c.add;
}

static __always_inline __u64 te_update_add_connect_domain(__u32 ip, pid_t pid,
							  __u32 domain_id)
{
	return te_update_add_endpoint_domain(TOP_CONNECT, ip, pid, domain_id);
}

static __always_inline __u64 te_update_add_recv_domain(__u32 ip, pid_t pid,
						       __u32 domain_id)
{
	return te_update_add_endpoint_domain(TOP_RECV, ip, pid, domain_id);
}

static __always_inline __u64 te_endpoint_stored_labels_domain(__u32 ip,
							      pid_t pid,
							      __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return 0;
	struct endp_domain_id edom = {};
	te_endp_domain_key_for(domain_id, ip, &edom);
	__u64 *el = bpf_map_lookup_elem(&ts_endp, &edom);
	return el ? *el : 0;
}

static __always_inline __u64 te_file_labels_domain(struct file_id *fid, const char *path,
						   pid_t pid, __u32 domain_id)
{
	struct file_domain_id *fdom = te_file_domain_tmp();
	if (!fdom)
		return te_update_add_file_domain(TOP_OPEN, path, pid, domain_id);
	if (!cap_domain_matches_pid(pid, domain_id))
		return 0;
	te_file_domain_key_for(domain_id, fid, fdom);
	struct file_state *fs = bpf_map_lookup_elem(&ts_file, fdom);

	return (fs ? fs->labels : 0) |
	       te_update_add_file_domain(TOP_OPEN, path, pid, domain_id);
}

static __always_inline __u64 te_file_stored_labels_domain(struct file_id *fid,
							  pid_t pid,
							  __u32 domain_id)
{
	struct file_domain_id *fdom = te_file_domain_tmp();
	if (!fdom)
		return 0;
	if (!cap_domain_matches_pid(pid, domain_id))
		return 0;
	te_file_domain_key_for(domain_id, fid, fdom);
	struct file_state *fs = bpf_map_lookup_elem(&ts_file, fdom);

	return fs ? fs->labels : 0;
}

/* read: proc absorbs file labels + file source; stamp `since read` invalidators. */
static __always_inline void te_read_domain(pid_t pid, struct file_id *fid,
					   const char *path, __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	struct file_domain_id *fdom = te_file_domain_tmp();
	if (!fdom)
		return;
	te_file_domain_key_for(domain_id, fid, fdom);
	struct file_state *fs = bpf_map_lookup_elem(&ts_file, fdom);
	__u64 file_labels = fs ? fs->labels : 0;
	struct te_update_ctx u = {
		.pid = pid,
		.domain_id = domain_id,
		.op = TOP_OPEN,
		.target = path,
	};

	te_collect_file_updates(&u);
	__u64 src_labels = u.add;
	te_add_labels_domain(pid, domain_id, file_labels | src_labels);
	if (file_labels)
		te_copy_file_prov_to_proc(pid, fdom, file_labels);
	if (src_labels)
		te_record_proc_prov_mask(pid, domain_id, src_labels, TOP_OPEN, path, 0);
	if (u.gates || u.invals) {
		pid_t r = te_root(pid);
		te_stamp(r, domain_id, te_tick(r, domain_id), u.gates, u.invals);
	}
}

static __always_inline void te_read(pid_t pid, struct file_id *fid, const char *path)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_read_domain(pid, fid, path, domain_id);
	}
}
/* write: file absorbs proc labels; stamp `since write` invalidators (this is
 * what makes a gate go stale when the agent edits an input again). The label
 * flow is gated on pl != 0, but invalidator stamping is not — editing an
 * unlabeled source file must still invalidate a prior gate. Layer A: when the
 * file does carry labels (so we already touch ts_file), record this write's
 * epoch as the object's last_write_epoch. */
static __always_inline void te_write_flow_domain(pid_t pid, struct file_id *fid,
						 const char *path, __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	struct te_update_ctx u = {
		.pid = pid,
		.domain_id = domain_id,
		.op = TOP_WRITE,
		.target = path,
	};
	te_collect_file_updates(&u);
	__u64 pl = te_labels_for_domain(pid, domain_id);
	if (!u.gates && !u.invals && !pl)
		return;
	pid_t r = te_root(pid);
	__u32 ep = te_tick(r, domain_id);
	if (u.gates || u.invals)
		te_stamp(r, domain_id, ep, u.gates, u.invals);
	if (pl) {
		struct file_domain_id *fdom = te_file_domain_tmp();
		if (!fdom)
			return;
		te_file_domain_key_for(domain_id, fid, fdom);
		struct file_state *fs = bpf_map_lookup_elem(&ts_file, fdom);
		if (fs) {
			fs->labels |= pl;
			fs->last_write_epoch = ep;
		} else {
			struct file_state ns = { .labels = pl, .last_write_epoch = ep };
			bpf_map_update_elem(&ts_file, fdom, &ns, BPF_ANY);
		}
		te_copy_proc_prov_to_file(pid, fdom, pl);
	}
}

static __always_inline void te_write_flow(pid_t pid, struct file_id *fid,
					  const char *path)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_write_flow_domain(pid, fid, path, domain_id);
	}
}

/* File sources are intrinsic object labels. Materialize a matching source path
 * into the file-object state so later reads through a renamed path inherit the
 * same label without tainting the process that merely renamed or wrote it. */
static __noinline void te_materialize_file_source_domain(pid_t pid,
							 struct file_id *fid,
							 const char *path,
							 __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	struct te_update_ctx u = {
		.pid = pid,
		.domain_id = domain_id,
		.op = TOP_OPEN,
		.target = path,
	};
	te_collect_file_updates(&u);
	__u64 src_labels = u.add;
	if (!src_labels)
		return;
	struct file_domain_id *fdom = te_file_domain_tmp();
	if (!fdom)
		return;
	te_file_domain_key_for(domain_id, fid, fdom);
	struct file_state *fs = bpf_map_lookup_elem(&ts_file, fdom);
	if (fs) {
		fs->labels |= src_labels;
	} else {
		struct file_state ns = { .labels = src_labels, .last_write_epoch = 0 };
		bpf_map_update_elem(&ts_file, fdom, &ns, BPF_ANY);
	}
}

static __noinline void te_materialize_file_source(pid_t pid,
						  struct file_id *fid,
						  const char *path)
{
	if (!fid)
		return;
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_materialize_file_source_domain(pid, fid, path, domain_id);
	}
}

static __noinline void te_copy_file_state_domain(pid_t pid,
						 struct file_id *from_fid,
						 struct file_id *to_fid,
						 __u32 domain_id,
						 __u32 delete_from)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	struct file_domain_id from = {};
	struct file_domain_id to = {};
	te_file_domain_key_for(domain_id, from_fid, &from);
	te_file_domain_key_for(domain_id, to_fid, &to);
	struct file_state *fs = bpf_map_lookup_elem(&ts_file, &from);
	__u32 have_src = fs ? 1 : 0;
	__u64 labels = fs ? fs->labels : 0;
	__u32 last_write_epoch = fs ? fs->last_write_epoch : 0;
	struct file_state *dst = bpf_map_lookup_elem(&ts_file, &to);
	if (delete_from) {
		__u64 dst_labels = dst ? dst->labels : 0;
		if (dst_labels)
			te_delete_file_prov_mask(&to, dst_labels);
		if (labels) {
			struct file_state ns = {
				.labels = labels,
				.last_write_epoch = last_write_epoch,
			};
			bpf_map_update_elem(&ts_file, &to, &ns, BPF_ANY);
			te_copy_file_prov_to_file(&from, &to, labels);
		} else {
			bpf_map_delete_elem(&ts_file, &to);
		}
		if (have_src)
			bpf_map_delete_elem(&ts_file, &from);
		if (labels)
			te_delete_file_prov_mask(&from, labels);
		return;
	}
	if (!labels)
		return;
	if (dst) {
		dst->labels |= labels;
		if (last_write_epoch > dst->last_write_epoch)
			dst->last_write_epoch = last_write_epoch;
	} else {
		struct file_state ns = {
			.labels = labels,
			.last_write_epoch = last_write_epoch,
		};
		bpf_map_update_elem(&ts_file, &to, &ns, BPF_ANY);
	}
	te_copy_file_prov_to_file(&from, &to, labels);
}

static __noinline void te_copy_file_state(pid_t pid,
					  struct file_id *from_fid,
					  struct file_id *to_fid,
					  __u32 delete_from)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_copy_file_state_domain(pid, from_fid, to_fid, domain_id,
					  delete_from);
	}
}

static __noinline void te_swap_file_state(pid_t pid,
					  struct file_id *a_fid,
					  struct file_id *b_fid)
{
	struct file_id tmp = {};

	tmp.ino = bpf_ktime_get_ns() ^ a_fid->ino ^ (b_fid->ino << 1);
	tmp.ino |= (1ULL << 63);
	tmp.dev = a_fid->dev ^ b_fid->dev ^ 0xffffffffU;
	tmp._pad = 0;
	if (tmp.ino == a_fid->ino && tmp.dev == a_fid->dev)
		tmp.ino ^= 0x517cc1b727220a95ULL;
	if (tmp.ino == b_fid->ino && tmp.dev == b_fid->dev)
		tmp.ino ^= 0x94d049bb133111ebULL;
	te_copy_file_state(pid, b_fid, &tmp, 1);
	te_copy_file_state(pid, a_fid, b_fid, 1);
	te_copy_file_state(pid, &tmp, a_fid, 1);
}

/* connect egress: endpoint records proc labels. */
static __always_inline void te_connect_flow_domain(__u32 ip, pid_t pid, __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	__u64 src_labels = te_update_add_connect_domain(ip, pid, domain_id);
	te_add_labels_domain(pid, domain_id, src_labels);
	if (src_labels)
		te_record_proc_prov_mask(pid, domain_id, src_labels, TOP_CONNECT, 0, ip);
	__u64 pl = te_labels_for_domain(pid, domain_id);
	if (!pl)
		return;
	struct endp_domain_id edom = {};
	te_endp_domain_key_for(domain_id, ip, &edom);
	__u64 *el = bpf_map_lookup_elem(&ts_endp, &edom);
	__u64 nv = (el ? *el : 0) | pl;
	bpf_map_update_elem(&ts_endp, &edom, &nv, BPF_ANY);
	te_copy_proc_prov_to_endp(pid, &edom, pl);
}

static __always_inline void te_connect_flow(__u32 ip, pid_t pid)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_connect_flow_domain(ip, pid, domain_id);
	}
}

/* recv ingress: process absorbs endpoint labels and recv endpoint sources. */
static __always_inline void te_recv_flow_domain(__u32 ip, pid_t pid, __u32 domain_id)
{
	if (!te_pid_active(pid) || !cap_domain_matches_pid(pid, domain_id))
		return;
	struct endp_domain_id edom = {};
	te_endp_domain_key_for(domain_id, ip, &edom);
	__u64 *el = bpf_map_lookup_elem(&ts_endp, &edom);
	__u64 endp_labels = el ? *el : 0;
	__u64 src_labels = te_update_add_recv_domain(ip, pid, domain_id);
	__u64 add = endp_labels | src_labels;
	if (!add)
		return;
	te_add_labels_domain(pid, domain_id, add);
	if (endp_labels)
		te_copy_endp_prov_to_proc(pid, &edom, endp_labels);
	if (src_labels)
		te_record_proc_prov_mask(pid, domain_id, src_labels, TOP_RECV, 0, ip);
}

static __always_inline void te_recv_flow(__u32 ip, pid_t pid)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_recv_flow_domain(ip, pid, domain_id);
	}
}

static __always_inline int te_cond_satisfied(const struct taint_rule *r,
					     struct te_rule_eval *e)
{
	if (r->cond_kind == TCOND_NONE)
		return 0;
	if (r->cond_kind == TCOND_LINEAGE) {
		struct proc_state *p = te_get_domain(e->pid, r->domain_id);
		return p && (p->lin_gates & r->gate);
	}
	if (r->cond_kind == TCOND_AFTER)
		return te_after_satisfied(r, e->pid);
	/* TCOND_TARGET */
	int m;
	if (e->op == TOP_CONNECT || e->op == TOP_RECV)
		m = ((e->ip & r->cond_ipv4_mask) == r->cond_ipv4);
	else if (e->op == TOP_EXEC)
		m = taint_exec_match(r->cond_match, e->target, r->cond_pat);
	else
		m = te_path_match(r->cond_match, e->target, r->cond_pat);
	return r->cond_neg ? !m : m;
}

/* Match event `e` against one rule index; returns the rule's effect (>=0) on a
 * hit, else -1, recording the matched rule_id in e->matched_rule. */
static __noinline int te_rule_effect(struct te_rule_eval *e, unsigned int idx)
{
	if (idx >= MAX_TAINT_RULES)
		return -1;
	__u32 key = idx;
	struct taint_rule *rp = bpf_map_lookup_elem(&ts_rules, &key);
	if (!rp)
		return -1;

	if (!cap_policy_rule_bound(rp->domain_id, idx))
		return -1;
	if (rp->op != e->op)
		return -1;
	if (rp->domain_id != 0 && rp->domain_id != e->current_domain_id &&
	    !cap_domain_matches_pid(e->pid, rp->domain_id))
		return -1;
	if (e->effect_mask) {
		if (rp->effect > TEFFECT_KILL)
			return -1;
		if (!(e->effect_mask & (1U << rp->effect)))
			return -1;
	}
	__u64 labels = 0;
	if (rp->domain_id == 0)
		labels = e->global_labels;
	else if (rp->domain_id == e->current_domain_id)
		labels = e->current_labels;
	else
		labels = te_labels_for_domain(e->pid, rp->domain_id);
	if (e->include_file_labels && e->has_fid)
		labels |= te_file_stored_labels_domain(&e->fid, e->pid, rp->domain_id);
	if (!taint_mask_ok(labels, rp->req, rp->forbid))
		return -1;
	if (e->op == TOP_CONNECT || e->op == TOP_RECV) {
		if ((e->ip & rp->ipv4_mask) != rp->ipv4)
			return -1;
	} else if (e->op == TOP_EXEC) {
		if (!taint_exec_match(rp->match, e->target, rp->target))
			return -1;
	} else if (!te_path_match(rp->match, e->target, rp->target)) {
		return -1;
	}
	if (e->op == TOP_EXEC && rp->arg[0] != '\0') {
		/* Match against the pre-tokenized arg slots. Re-look-up the per-CPU
		 * scratch here so the verifier keeps the map_value bound. */
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, rp->arg))
			return -1;
	}
	if (te_cond_satisfied(rp, e))
		return -1;
	e->matched_rule = (int)rp->rule_id;
	e->matched_index = (int)idx;
	e->matched_domain_id = rp->domain_id;
	e->matched_req = rp->req;
	e->matched_labels = labels & rp->req;
	return (int)rp->effect;
}

static __noinline int te_rule_effect_exec_simple(struct te_rule_eval *e,
						 unsigned int idx)
{
	if (idx >= MAX_TAINT_RULES)
		return -1;
	__u32 key = idx;
	struct taint_rule *rp = bpf_map_lookup_elem(&ts_rules, &key);
	if (!rp)
		return -1;

	if (!cap_policy_rule_bound(rp->domain_id, idx))
		return -1;
	if (rp->op != TOP_EXEC)
		return -1;
	if (rp->cond_kind != TCOND_NONE)
		return -1;
	if (rp->domain_id != 0 && rp->domain_id != e->current_domain_id &&
	    !cap_domain_matches_pid(e->pid, rp->domain_id))
		return -1;
	if (e->effect_mask) {
		if (rp->effect > TEFFECT_KILL)
			return -1;
		if (!(e->effect_mask & (1U << rp->effect)))
			return -1;
	}
	__u64 labels = 0;
	if (rp->domain_id == 0)
		labels = e->global_labels;
	else if (rp->domain_id == e->current_domain_id)
		labels = e->current_labels;
	else
		labels = te_labels_for_domain(e->pid, rp->domain_id);
	if (!taint_mask_ok(labels, rp->req, rp->forbid))
		return -1;
	if (!te_exec_simple_match(rp->match, e->target, rp->target))
		return -1;
	if (rp->arg[0] != '\0') {
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, rp->arg))
			return -1;
	}
	e->matched_rule = (int)rp->rule_id;
	e->matched_index = (int)idx;
	e->matched_domain_id = rp->domain_id;
	e->matched_req = rp->req;
	e->matched_labels = labels & rp->req;
	return (int)rp->effect;
}

static __noinline int te_rule_effect_exec_complex(struct te_rule_eval *e,
						  unsigned int idx)
{
	if (idx >= MAX_TAINT_RULES)
		return -1;
	__u32 key = idx;
	struct taint_rule *rp = bpf_map_lookup_elem(&ts_rules, &key);
	if (!rp)
		return -1;

	if (!cap_policy_rule_bound(rp->domain_id, idx))
		return -1;
	if (rp->op != TOP_EXEC)
		return -1;
	if ((rp->match == TAINT_MATCH_EXACT || rp->match == TAINT_MATCH_ANY) &&
	    rp->cond_kind == TCOND_NONE)
		return -1;
	if (rp->domain_id != 0 && rp->domain_id != e->current_domain_id &&
	    !cap_domain_matches_pid(e->pid, rp->domain_id))
		return -1;
	if (e->effect_mask) {
		if (rp->effect > TEFFECT_KILL)
			return -1;
		if (!(e->effect_mask & (1U << rp->effect)))
			return -1;
	}
	__u64 labels = 0;
	if (rp->domain_id == 0)
		labels = e->global_labels;
	else if (rp->domain_id == e->current_domain_id)
		labels = e->current_labels;
	else
		labels = te_labels_for_domain(e->pid, rp->domain_id);
	if (!taint_mask_ok(labels, rp->req, rp->forbid))
		return -1;
	if (!taint_exec_match(rp->match, e->target, rp->target))
		return -1;
	if (rp->arg[0] != '\0') {
		struct te_argslots *a = te_argslots_buf();
		if (!a || !taint_arg_match(a->slots, rp->arg))
			return -1;
	}
	if (te_cond_satisfied(rp, e))
		return -1;
	e->matched_rule = (int)rp->rule_id;
	e->matched_index = (int)idx;
	e->matched_domain_id = rp->domain_id;
	e->matched_req = rp->req;
	e->matched_labels = labels & rp->req;
	return (int)rp->effect;
}

static __noinline int te_rule_effect_no_args(struct te_rule_eval *e,
					     unsigned int idx)
{
	if (idx >= MAX_TAINT_RULES)
		return -1;
	__u32 key = idx;
	struct taint_rule *rp = bpf_map_lookup_elem(&ts_rules, &key);
	if (!rp)
		return -1;

	if (!cap_policy_rule_bound(rp->domain_id, idx))
		return -1;
	if (rp->op != e->op)
		return -1;
	if (rp->domain_id != 0 && rp->domain_id != e->current_domain_id &&
	    !cap_domain_matches_pid(e->pid, rp->domain_id))
		return -1;
	if (e->effect_mask) {
		if (rp->effect > TEFFECT_KILL)
			return -1;
		if (!(e->effect_mask & (1U << rp->effect)))
			return -1;
	}
	__u64 labels = 0;
	if (rp->domain_id == 0)
		labels = e->global_labels;
	else if (rp->domain_id == e->current_domain_id)
		labels = e->current_labels;
	else
		labels = te_labels_for_domain(e->pid, rp->domain_id);
	if (e->include_file_labels && e->has_fid)
		labels |= te_file_stored_labels_domain(&e->fid, e->pid, rp->domain_id);
	if (!taint_mask_ok(labels, rp->req, rp->forbid))
		return -1;
	if (e->op == TOP_EXEC) {
		if (rp->arg[0] != '\0')
			return -1;
		if (!taint_exec_match(rp->match, e->target, rp->target))
			return -1;
	} else {
		return -1;
	}
	if (te_cond_satisfied(rp, e))
		return -1;
	e->matched_rule = (int)rp->rule_id;
	e->matched_index = (int)idx;
	e->matched_domain_id = rp->domain_id;
	e->matched_req = rp->req;
	e->matched_labels = labels & rp->req;
	return (int)rp->effect;
}

struct te_rule_ctx {
	struct te_rule_eval *e;
	unsigned int best_effect;
	int best_rule;
	int best_idx;
	__u32 best_domain_id;
	__u64 best_req;
	__u64 best_labels;
};
static int te_rule_cb(__u32 i, void *vc)
{
	struct te_rule_ctx *c = vc;
	int eff = te_rule_effect(c->e, i);
	if (eff < 0)
		return 0;
	if (c->best_rule < 0 || (unsigned int)eff > c->best_effect) {
		c->best_rule = c->e->matched_rule;
		c->best_idx = c->e->matched_index;
		c->best_effect = (unsigned int)eff;
		c->best_domain_id = c->e->matched_domain_id;
		c->best_req = c->e->matched_req;
		c->best_labels = c->e->matched_labels;
		if (c->best_effect == TEFFECT_KILL)
			return 1;
	}
	return 0;
}

static int te_rule_no_args_cb(__u32 i, void *vc)
{
	struct te_rule_ctx *c = vc;
	int eff = te_rule_effect_no_args(c->e, i);
	if (eff < 0)
		return 0;
	if (c->best_rule < 0 || (unsigned int)eff > c->best_effect) {
		c->best_rule = c->e->matched_rule;
		c->best_idx = c->e->matched_index;
		c->best_effect = (unsigned int)eff;
		c->best_domain_id = c->e->matched_domain_id;
		c->best_req = c->e->matched_req;
		c->best_labels = c->e->matched_labels;
		if (c->best_effect == TEFFECT_KILL)
			return 1;
	}
	return 0;
}

static int te_rule_exec_simple_cb(__u32 i, void *vc)
{
	struct te_rule_ctx *c = vc;
	int eff = te_rule_effect_exec_simple(c->e, i);
	if (eff < 0)
		return 0;
	if (c->best_rule < 0 || (unsigned int)eff > c->best_effect) {
		c->best_rule = c->e->matched_rule;
		c->best_idx = c->e->matched_index;
		c->best_effect = (unsigned int)eff;
		c->best_domain_id = c->e->matched_domain_id;
		c->best_req = c->e->matched_req;
		c->best_labels = c->e->matched_labels;
		if (c->best_effect == TEFFECT_KILL)
			return 1;
	}
	return 0;
}

static int te_rule_exec_complex_cb(__u32 i, void *vc)
{
	struct te_rule_ctx *c = vc;
	int eff = te_rule_effect_exec_complex(c->e, i);
	if (eff < 0)
		return 0;
	if (c->best_rule < 0 || (unsigned int)eff > c->best_effect) {
		c->best_rule = c->e->matched_rule;
		c->best_idx = c->e->matched_index;
		c->best_effect = (unsigned int)eff;
		c->best_domain_id = c->e->matched_domain_id;
		c->best_req = c->e->matched_req;
		c->best_labels = c->e->matched_labels;
		if (c->best_effect == TEFFECT_KILL)
			return 1;
	}
	return 0;
}

/* Evaluate sinks for one normalized event. Returns the matched rule_id, or -1.
 * The rule loop runs via bpf_loop() with the count from the ts_counts map, so
 * the callback is verified ONCE — verifier cost is independent of rule count,
 * which (with the branchless matchers) lets 100+ rules load in one program. */
static __noinline int te_check_labels(struct te_rule_eval *e)
{
	struct te_rule_ctx c = {
		.e = e,
		.best_effect = TEFFECT_NOTIFY,
		.best_rule = -1,
		.best_idx = -1,
	};
	bpf_loop(te_count(0), te_rule_cb, &c, 0);
	if (c.best_rule >= 0) {
		e->effect = c.best_effect;
		e->matched_rule = c.best_rule;
		e->matched_index = c.best_idx;
		e->matched_domain_id = c.best_domain_id;
		e->matched_req = c.best_req;
		e->matched_labels = c.best_labels;
	}
	return c.best_rule;
}

static __noinline int te_check_exec_simple(struct te_rule_eval *e)
{
	struct te_rule_ctx c = {
		.e = e,
		.best_effect = TEFFECT_NOTIFY,
		.best_rule = -1,
		.best_idx = -1,
	};
	bpf_loop(te_count(0), te_rule_exec_simple_cb, &c, 0);
	if (c.best_rule >= 0) {
		e->effect = c.best_effect;
		e->matched_rule = c.best_rule;
		e->matched_index = c.best_idx;
		e->matched_domain_id = c.best_domain_id;
		e->matched_req = c.best_req;
		e->matched_labels = c.best_labels;
	}
	return c.best_rule;
}

static __noinline int te_check_exec_complex(struct te_rule_eval *e)
{
	struct te_rule_ctx c = {
		.e = e,
		.best_effect = TEFFECT_NOTIFY,
		.best_rule = -1,
		.best_idx = -1,
	};
	bpf_loop(te_count(0), te_rule_exec_complex_cb, &c, 0);
	if (c.best_rule >= 0) {
		e->effect = c.best_effect;
		e->matched_rule = c.best_rule;
		e->matched_index = c.best_idx;
		e->matched_domain_id = c.best_domain_id;
		e->matched_req = c.best_req;
		e->matched_labels = c.best_labels;
	}
	return c.best_rule;
}

static __noinline int te_check_labels_no_args(struct te_rule_eval *e)
{
	struct te_rule_ctx c = {
		.e = e,
		.best_effect = TEFFECT_NOTIFY,
		.best_rule = -1,
		.best_idx = -1,
	};
	bpf_loop(te_count(0), te_rule_no_args_cb, &c, 0);
	if (c.best_rule >= 0) {
		e->effect = c.best_effect;
		e->matched_rule = c.best_rule;
		e->matched_index = c.best_idx;
		e->matched_domain_id = c.best_domain_id;
		e->matched_req = c.best_req;
		e->matched_labels = c.best_labels;
	}
	return c.best_rule;
}

static __always_inline int te_exit_status_matches(int raw_status, int expected)
{
	if (expected < 0 || expected > 255)
		return 0;
	if (raw_status & 0x7f)
		return 0;
	return (((unsigned int)raw_status >> 8) & 0xff) == (unsigned int)expected;
}

struct te_exit_gate_ctx {
	pid_t pid;
	__u32 domain_id;
	__u64 pending;
	int raw_status;
	__u64 gates;
};

static int te_exit_gate_cb(__u32 i, void *vc)
{
	struct te_exit_gate_ctx *c = vc;

	if (i >= MAX_TAINT_UPDATES)
		return 1;
	struct taint_update *up = bpf_map_lookup_elem(&ts_updates, &i);
	if (!up)
		return 1;
	struct taint_update u = *up;
	__u64 matched = c->pending & u.gates;
	if (!matched || u.gate_exit_code == TAINT_GATE_IMMEDIATE)
		return 0;
	if (u.domain_id != c->domain_id)
		return 0;
	if (te_exit_status_matches(c->raw_status, u.gate_exit_code))
		c->gates |= matched;
	return 0;
}

static __noinline __u64 te_exit_gate_hits(pid_t pid, __u32 domain_id,
					  __u64 pending, int raw_status)
{
	struct te_exit_gate_ctx c = {
		.pid = pid,
		.domain_id = domain_id,
		.pending = pending,
		.raw_status = raw_status,
		.gates = 0,
	};

	bpf_loop(te_count(1), te_exit_gate_cb, &c, 0);
	return c.gates;
}

static __always_inline void te_delete_proc_domain(pid_t pid, __u32 domain_id)
{
	if (domain_id == 0) {
		bpf_map_delete_elem(&ts_proc, &pid);
	} else {
		struct pid_domain_id pk = {};
		te_pid_domain_key(pid, domain_id, &pk);
		bpf_map_delete_elem(&ts_proc_domains, &pk);
	}
	for (int i = 0; i < MAX_TAINT_LABELS; i++) {
		struct pid_label_id key = {
			.pid = pid,
			.domain_id = domain_id,
			.label = 1ULL << i,
		};
		bpf_map_delete_elem(&ts_proc_prov, &key);
	}
}

static __always_inline void te_exit_domain(pid_t pid, int raw_status, __u32 domain_id)
{
	struct pid_domain_id key = {};
	te_pid_domain_key(pid, domain_id, &key);
	struct te_exit_gate_pending *pending = bpf_map_lookup_elem(&ts_exit_gates, &key);
	if (pending && pending->gates) {
		__u64 gates = te_exit_gate_hits(pid, domain_id, pending->gates, raw_status);
		if (gates) {
			pid_t r = te_root(pid);
			te_stamp(r, domain_id, pending->epoch, gates, 0);
		}
	}
	bpf_map_delete_elem(&ts_exit_gates, &key);
	te_delete_proc_domain(pid, domain_id);
}

static __always_inline void te_exit(pid_t pid, int raw_status)
{
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		__u32 domain_id = te_domain_for_depth(pid, i);
		if (i > 0 && !domain_id)
			break;
		te_exit_domain(pid, raw_status, domain_id);
	}
	bpf_map_delete_elem(&ts_root, &pid);
	cap_exit(pid);
}

#endif /* __TAINT_ENGINE_BPF_H */
