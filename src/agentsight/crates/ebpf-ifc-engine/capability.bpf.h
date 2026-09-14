/* SPDX-License-Identifier: GPL-2.0 OR BSD-3-Clause */
/* Copyright (c) 2026 eunomia-bpf org. */
#ifndef __CAPABILITY_BPF_H
#define __CAPABILITY_BPF_H

/*
 * Minimal runtime admission path for policy deltas.
 *
 * User space can enqueue cap_delta_request records into cap_req. The kernel
 * drains them from a dedicated getpid tracepoint tick, checks only
 * masks/scope/target, and applies accepted monotonic deltas to cap_state. No
 * DSL/YAML/roles live here.
 */

#define AUTH_TARGET_SELF  (1ULL << 0)
#define AUTH_TARGET_CHILD (1ULL << 1)

#define AUTH_ADD_RESTRICTION (1ULL << 0)
#define AUTH_ADD_LABEL       (1ULL << 1)
#define AUTH_REQUIRE_GATE    (1ULL << 2)
#define AUTH_NARROW_SCOPE    (1ULL << 3)
#define AUTH_BIND_RULE       (1ULL << 4)
#define AUTH_DECLASSIFY      (1ULL << 5)
#define AUTH_DELEGATE        (1ULL << 6)

#define CAP_STAT_ACCEPT 0
#define CAP_STAT_REJECT 1
#define CAP_STAT_DRAIN  2
#define CAP_STAT_DROP   3
#define CAP_DOMAIN_DEPTH 8

#ifndef TE_POLICY_PATH_CONTAINS
#define TE_POLICY_PATH_CONTAINS (1U << 0)
#define TE_POLICY_PATH_SUFFIX   (1U << 1)
#define TE_POLICY_OPEN_RULES    (1U << 2)
#define TE_POLICY_WRITE_RULES   (1U << 3)
#define TE_POLICY_CONNECT       (1U << 4)
#define TE_POLICY_RECV          (1U << 5)
#define TE_POLICY_FILE_FLOW     (1U << 6)
#define TE_POLICY_BLOCK_EXEC    (1U << 7)
#define TE_POLICY_BLOCK_FILE    (1U << 8)
#define TE_POLICY_BLOCK_CONNECT (1U << 9)
#endif

struct cap_state {
	__u32 parent;
	__u32 scope_id;
	__u64 labels;
	__u64 authority_mask;
	__u64 target_mask;
	__u64 restrict_mask;
	__u64 gate_mask;
	__u64 label_mask;
};

struct cap_policy_mask {
	__u64 lo;
	__u64 hi;
};

struct cap_delta_request {
	pid_t caller_pid;
	__u32 target_id;
	__u32 new_scope_id;
	__u64 required_mask;
	__u64 add_restrict_mask;
	__u64 add_label_mask;
	__u64 add_gate_mask;
};

struct {
	__uint(type, BPF_MAP_TYPE_USER_RINGBUF);
	__uint(max_entries, 64 * 1024);
} cap_req SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 4096);
	__type(key, __u32);
	__type(value, struct cap_state);
} cap_state SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 16384);
	__type(key, pid_t);
	__type(value, __u32);
} cap_task SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 4096);
	__type(key, __u32);
	__type(value, struct cap_policy_mask);
} cap_policy SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 4);
	__type(key, __u32);
	__type(value, __u64);
} cap_stats SEC(".maps");

/* The pid that declared the next drain: userspace writes its own pid before
 * submitting to cap_req. The drain hook refuses to run for any other pid, so
 * a foreign syscall on the trigger tracepoint cannot consume the submitter's
 * records in a foreign capability context.
 */
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, 1);
	__type(key, __u32);
	__type(value, __s32);
} cap_pending_submitter SEC(".maps");

static __always_inline void cap_count(__u32 slot)
{
	__u64 *v = bpf_map_lookup_elem(&cap_stats, &slot);
	if (v)
		(*v)++;
}

static __always_inline __u64
cap_required_mask_fields(__u64 required_mask, __u64 add_restrict_mask,
			 __u64 add_label_mask, __u64 del_label_mask,
			 __u64 add_gate_mask, __u32 new_scope_id)
{
	__u64 mask = required_mask;

	if (add_restrict_mask)
		mask |= AUTH_ADD_RESTRICTION;
	if (add_label_mask)
		mask |= AUTH_ADD_LABEL;
	if (del_label_mask)
		mask |= AUTH_DECLASSIFY;
	if (add_gate_mask)
		mask |= AUTH_REQUIRE_GATE;
	if (new_scope_id)
		mask |= AUTH_NARROW_SCOPE;
	return mask;
}

static __always_inline __u64 cap_required_mask(const struct cap_delta_request *r)
{
	return cap_required_mask_fields(r->required_mask, r->add_restrict_mask,
					r->add_label_mask, 0, r->add_gate_mask,
					r->new_scope_id);
}

static __always_inline int cap_scope_subset(__u32 new_scope, __u32 old_scope)
{
	if (!new_scope)
		return 1;
	if (!old_scope)
		return 1;
	return new_scope >= old_scope;
}

static __always_inline __u64
cap_target_mask(__u32 caller, __u32 target, const struct cap_state *dst)
{
	__u64 mask = 0;

	if (caller == target)
		mask |= AUTH_TARGET_SELF;
	if (dst->parent == caller)
		mask |= AUTH_TARGET_CHILD;
	return mask;
}

static __always_inline int
cap_check_fields(pid_t caller_pid, __u32 target_id, __u32 new_scope_id,
		 __u64 required_mask, __u64 add_restrict_mask,
		 __u64 add_label_mask, __u64 del_label_mask,
		 __u64 add_gate_mask)
{
	__u32 *caller = bpf_map_lookup_elem(&cap_task, &caller_pid);
	if (!caller)
		return -1;

	struct cap_state *src = bpf_map_lookup_elem(&cap_state, caller);
	struct cap_state *dst = bpf_map_lookup_elem(&cap_state, &target_id);
	if (!src || !dst)
		return -1;

	__u64 target_mask = cap_target_mask(*caller, target_id, dst);
	if (!(target_mask & src->target_mask))
		return -1;
	if (cap_required_mask_fields(required_mask, add_restrict_mask,
				     add_label_mask, del_label_mask, add_gate_mask,
				     new_scope_id) & ~src->authority_mask)
		return -1;
	if ((add_label_mask | del_label_mask) & ~src->label_mask)
		return -1;
	if (!cap_scope_subset(new_scope_id, dst->scope_id))
		return -1;
	return 0;
}

static __always_inline int cap_check_request(const struct cap_delta_request *r)
{
	return cap_check_fields(r->caller_pid, r->target_id, r->new_scope_id,
				r->required_mask, r->add_restrict_mask,
				r->add_label_mask, 0, r->add_gate_mask);
}

static __always_inline int cap_can_submit_for(pid_t submitter_pid, pid_t caller_pid)
{
	if (submitter_pid == caller_pid)
		return 1;

	__u32 *submitter = bpf_map_lookup_elem(&cap_task, &submitter_pid);
	if (!submitter)
		return 0;
	struct cap_state *src = bpf_map_lookup_elem(&cap_state, submitter);
	if (!src)
		return 0;
	return (src->authority_mask & AUTH_DELEGATE) != 0;
}

static __always_inline int cap_path_match_supported(unsigned char match)
{
	if (match == TAINT_MATCH_CONTAINS &&
	    !(policy_features & TE_POLICY_PATH_CONTAINS))
		return 0;
	if (match == TAINT_MATCH_SUFFIX &&
	    !(policy_features & TE_POLICY_PATH_SUFFIX))
		return 0;
	return 1;
}

static __always_inline int cap_policy_mask_has(const struct cap_policy_mask *m,
					       __u32 idx)
{
	if (!m || idx >= MAX_TAINT_RULES)
		return 0;
	if (idx < 64)
		return (m->lo & (1ULL << idx)) != 0;
	return (m->hi & (1ULL << (idx - 64))) != 0;
}

static __always_inline int cap_policy_rule_bound(__u32 domain_id, __u32 idx)
{
	struct cap_policy_mask *m = bpf_map_lookup_elem(&cap_policy, &domain_id);

	return cap_policy_mask_has(m, idx);
}

static __always_inline int cap_policy_bind_rule(__u32 domain_id, __u32 idx)
{
	if (idx >= MAX_TAINT_RULES)
		return -1;

	struct cap_policy_mask next = {};
	struct cap_policy_mask *cur = bpf_map_lookup_elem(&cap_policy, &domain_id);
	if (cur)
		next = *cur;
	if (idx < 64)
		next.lo |= 1ULL << idx;
	else
		next.hi |= 1ULL << (idx - 64);
	return bpf_map_update_elem(&cap_policy, &domain_id, &next, BPF_ANY);
}

static __always_inline void cap_policy_clear_domain(__u32 domain_id)
{
	bpf_map_delete_elem(&cap_policy, &domain_id);
}

static __always_inline int cap_update_supported(const struct taint_update *u)
{
	if (u->op == TOP_OPEN || u->op == TOP_WRITE) {
		if (!(policy_features & TE_POLICY_FILE_FLOW))
			return 0;
		return cap_path_match_supported(u->match);
	}
	if (u->op == TOP_CONNECT && !(policy_features & TE_POLICY_CONNECT))
		return 0;
	if (u->op == TOP_RECV && !(policy_features & TE_POLICY_RECV))
		return 0;
	return 1;
}

static __always_inline int cap_rule_supported(const struct taint_rule *r)
{
	if (r->effect == TEFFECT_BLOCK) {
		if (r->op == TOP_EXEC && r->arg[0] == '\0' &&
		    !(policy_features & TE_POLICY_BLOCK_EXEC))
			return 0;
		if ((r->op == TOP_OPEN || r->op == TOP_WRITE) &&
		    !(policy_features & TE_POLICY_BLOCK_FILE))
			return 0;
		if (r->op == TOP_CONNECT &&
		    !(policy_features & TE_POLICY_BLOCK_CONNECT))
			return 0;
	}
	if (r->op == TOP_OPEN) {
		if (!(policy_features & TE_POLICY_OPEN_RULES))
			return 0;
		if (!cap_path_match_supported(r->match))
			return 0;
		if (r->cond_kind == TCOND_TARGET &&
		    !cap_path_match_supported(r->cond_match))
			return 0;
	}
	if (r->op == TOP_WRITE) {
		if (!(policy_features & TE_POLICY_WRITE_RULES))
			return 0;
		if (!cap_path_match_supported(r->match))
			return 0;
		if (r->cond_kind == TCOND_TARGET &&
		    !cap_path_match_supported(r->cond_match))
			return 0;
	}
	if (r->op == TOP_CONNECT && !(policy_features & TE_POLICY_CONNECT))
		return 0;
	if (r->op == TOP_RECV && !(policy_features & TE_POLICY_RECV))
		return 0;
	return 1;
}

static __always_inline int cap_apply_request(const struct cap_delta_request *r)
{
	if (cap_check_request(r) != 0)
		return -1;

	struct cap_state *dst = bpf_map_lookup_elem(&cap_state, &r->target_id);
	if (!dst)
		return -1;

	struct cap_state next = *dst;
	next.restrict_mask |= r->add_restrict_mask;
	next.labels |= r->add_label_mask;
	next.gate_mask |= r->add_gate_mask;
	if (r->new_scope_id)
		next.scope_id = r->new_scope_id;
	bpf_map_update_elem(&cap_state, &r->target_id, &next, BPF_EXIST);
	return 0;
}

/* Runtime policy deltas use negative tags so they cannot be confused with
 * capability updates, whose first field is always a positive caller pid.
 */
#define CAP_REQ_APPEND_UPDATE  (-4)
#define CAP_REQ_APPEND_RULE    (-5)

struct cap_append_update {
	__s32 tag;
	pid_t caller_pid;
	__u32 target_id;
	__u32 new_scope_id;
	__u64 required_mask;
	struct taint_update entry;
};

struct cap_append_rule {
	__s32 tag;
	pid_t caller_pid;
	__u32 target_id;
	__u32 new_scope_id;
	__u64 required_mask;
	struct taint_rule entry;
};

struct cap_drain_ctx {
	pid_t current_pid;
};

static __always_inline int
cap_append_update_admit(const struct cap_append_update *r, pid_t current_pid)
{
	if (!cap_can_submit_for(current_pid, r->caller_pid))
		return -1;
	if (!cap_update_supported(&r->entry))
		return -1;

	return cap_check_fields(r->caller_pid, r->target_id, r->new_scope_id,
				r->required_mask, 0, r->entry.add, r->entry.del,
				r->entry.gates | r->entry.invals);
}

static __always_inline int
cap_append_rule_admit(const struct cap_append_rule *r, pid_t current_pid)
{
	if (!cap_can_submit_for(current_pid, r->caller_pid))
		return -1;
	if (!cap_rule_supported(&r->entry))
		return -1;

	return cap_check_fields(r->caller_pid, r->target_id, r->new_scope_id,
				r->required_mask | AUTH_BIND_RULE,
				0, 0, 0, 0);
}

static __always_inline int cap_append_update_entry(const struct cap_append_update *r)
{
	__u32 slot = 1;
	__u32 *count = bpf_map_lookup_elem(&ts_counts, &slot);
	if (!count || *count >= MAX_TAINT_UPDATES)
		return -1;

	struct taint_update entry = r->entry;
	entry.domain_id = r->target_id;
	__u32 idx = *count;
	if (bpf_map_update_elem(&ts_updates, &idx, &entry, BPF_ANY) != 0)
		return -1;
	idx++;
	bpf_map_update_elem(&ts_counts, &slot, &idx, BPF_ANY);
	return 0;
}

static __always_inline int cap_append_rule_entry(const struct cap_append_rule *r)
{
	__u32 slot = 0;
	__u32 *count = bpf_map_lookup_elem(&ts_counts, &slot);
	if (!count || *count >= MAX_TAINT_RULES)
		return -1;

	struct taint_rule entry = r->entry;
	entry.domain_id = r->target_id;
	__u32 idx = *count;
	if (bpf_map_update_elem(&ts_rules, &idx, &entry, BPF_ANY) != 0)
		return -1;
	if (cap_policy_bind_rule(r->target_id, idx) != 0)
		return -1;
	idx++;
	bpf_map_update_elem(&ts_counts, &slot, &idx, BPF_ANY);
	return 0;
}

static long cap_request_cb(struct bpf_dynptr *dynptr, void *data)
{
	const __s32 *tag = bpf_dynptr_data(dynptr, 0, sizeof(__s32));
	if (!tag) {
		cap_count(CAP_STAT_DROP);
		return 0;
	}

	if (*tag == CAP_REQ_APPEND_UPDATE) {
		const struct cap_append_update *r =
			bpf_dynptr_data(dynptr, 0, sizeof(*r));
		struct cap_drain_ctx *ctx = data;
		if (!r || cap_append_update_admit(r, ctx->current_pid) != 0 ||
		    cap_append_update_entry(r) != 0) {
			cap_count(CAP_STAT_REJECT);
			return 0;
		}
		cap_count(CAP_STAT_ACCEPT);
		return 0;
	}
	if (*tag == CAP_REQ_APPEND_RULE) {
		const struct cap_append_rule *r =
			bpf_dynptr_data(dynptr, 0, sizeof(*r));
		struct cap_drain_ctx *ctx = data;
		if (!r || cap_append_rule_admit(r, ctx->current_pid) != 0 ||
		    cap_append_rule_entry(r) != 0) {
			cap_count(CAP_STAT_REJECT);
			return 0;
		}
		cap_count(CAP_STAT_ACCEPT);
		return 0;
	}

	/* Normal capability delta request (caller_pid > 0). */
	{
		const struct cap_delta_request *r;
		struct cap_drain_ctx *ctx = data;

		r = bpf_dynptr_data(dynptr, 0, sizeof(*r));
		if (!r) {
			cap_count(CAP_STAT_DROP);
			return 0;
		}
		if (r->caller_pid != ctx->current_pid) {
			cap_count(CAP_STAT_DROP);
			return 0;
		}
		if (cap_apply_request(r) == 0)
			cap_count(CAP_STAT_ACCEPT);
		else
			cap_count(CAP_STAT_REJECT);
	}
	return 0;
}

static __always_inline void cap_drain_current(void)
{
	__u32 slot = 0;
	__s32 *pending = bpf_map_lookup_elem(&cap_pending_submitter, &slot);
	__s32 current = bpf_get_current_pid_tgid() >> 32;

	if (!pending || *pending != current)
		return;

	struct cap_drain_ctx ctx = {
		.current_pid = current,
	};
	bpf_user_ringbuf_drain(&cap_req, cap_request_cb, &ctx, 0);
	cap_count(CAP_STAT_DRAIN);
}

static __always_inline __u64 cap_labels_for_pid(pid_t pid)
{
	__u32 *target = bpf_map_lookup_elem(&cap_task, &pid);
	if (!target)
		return 0;
	struct cap_state *p = bpf_map_lookup_elem(&cap_state, target);
	if (!p)
		return 0;
	return p->labels | p->restrict_mask;
}

static __always_inline int cap_task_bound(pid_t pid)
{
	__u32 *target = bpf_map_lookup_elem(&cap_task, &pid);

	return target && *target != 0;
}

static __always_inline __u32 cap_domain_for_pid(pid_t pid)
{
	__u32 *target = bpf_map_lookup_elem(&cap_task, &pid);

	return target ? *target : 0;
}

static __always_inline __u32 cap_parent_domain(__u32 domain_id)
{
	if (!domain_id)
		return 0;
	struct cap_state *state = bpf_map_lookup_elem(&cap_state, &domain_id);
	if (!state)
		return 0;
	return state->parent;
}

static __always_inline int cap_domain_matches_pid(pid_t pid, __u32 domain_id)
{
	if (!domain_id)
		return 1;

	__u32 *target = bpf_map_lookup_elem(&cap_task, &pid);
	if (!target || !*target)
		return 0;

	__u32 current = *target;
	for (int i = 0; i < CAP_DOMAIN_DEPTH; i++) {
		if (current == domain_id)
			return 1;
		struct cap_state *state = bpf_map_lookup_elem(&cap_state, &current);
		if (!state || !state->parent)
			return 0;
		current = state->parent;
	}
	return 0;
}

static __always_inline __u64 cap_labels_for_pid_domain(pid_t pid, __u32 domain_id)
{
	if (!domain_id)
		return 0;
	if (!cap_domain_matches_pid(pid, domain_id))
		return 0;
	struct cap_state *p = bpf_map_lookup_elem(&cap_state, &domain_id);
	if (!p)
		return 0;
	return p->labels | p->restrict_mask;
}

static __always_inline void cap_fork(pid_t ppid, pid_t cpid)
{
	__u32 *target = bpf_map_lookup_elem(&cap_task, &ppid);
	if (target)
		bpf_map_update_elem(&cap_task, &cpid, target, BPF_ANY);
}

static __always_inline void cap_exit(pid_t pid)
{
	bpf_map_delete_elem(&cap_task, &pid);
}

#endif /* __CAPABILITY_BPF_H */
