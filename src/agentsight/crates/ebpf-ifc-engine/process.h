/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
/* Copyright (c) 2026 eunomia-bpf org. */
#ifndef __PROCESS_H
#define __PROCESS_H

#define TASK_COMM_LEN 16
#define MAX_FILENAME_LEN 127
/* Event-only operation codes for inode-guard denials; rules never use them. */
#define TE_EVENT_OP_UNLINK 5
#define TE_EVENT_OP_RENAME 6

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

#include "taint.h"

/* Backend modes: how the running hook evaluates a matched rule. Tracepoint
 * backends observe operations after the fact and can only report or signal;
 * BPF LSM hooks additionally deny in line. */
#define TE_MODE_NOTIFY      0
#define TE_MODE_BLOCK       1
#define TE_MODE_KILL        2
#define TE_MODE_UNSUPPORTED 3

/* Policy feature bit gating the pre-operation (BPF LSM) hook able to deny
 * `op`. TOP_RECV has no dedicated block bit: its recvmsg hook is attached
 * whenever recv sources or rules are enabled. 0 when no hook can deny it. */
static __always_inline unsigned int te_block_feature_bit(unsigned int op)
{
	switch (op) {
	case TOP_EXEC:
		return TE_POLICY_BLOCK_EXEC;
	case TOP_OPEN:
	case TOP_WRITE:
		return TE_POLICY_BLOCK_FILE;
	case TOP_CONNECT:
		return TE_POLICY_BLOCK_CONNECT;
	case TOP_RECV:
		return TE_POLICY_RECV;
	default:
		return 0;
	}
}

/* Whether the loaded engine attached the hook that denies `op` before it
 * happens. `features` and `enforce` mirror the eBPF globals `policy_features`
 * and `enforce_mode` (0 when BPF LSM was inactive at load time). */
static __always_inline int te_block_hook_attached(unsigned int op,
						  unsigned int features,
						  unsigned int enforce)
{
	return enforce != 0 && (features & te_block_feature_bit(op)) != 0;
}

/* Action the backend takes for a matched rule `effect`. A block rule on a
 * notify-only backend degrades to a notify action: the match is reported as
 * an unblocked violation instead of being silently skipped. */
static __always_inline unsigned int te_effect_mode(unsigned int backend_mode,
						   unsigned int effect)
{
	if (effect == TEFFECT_NOTIFY)
		return TE_MODE_NOTIFY;
	if (effect == TEFFECT_KILL)
		return TE_MODE_KILL;
	if (effect == TEFFECT_BLOCK)
		return backend_mode == TE_MODE_BLOCK ? TE_MODE_BLOCK
						     : TE_MODE_NOTIFY;
	return TE_MODE_UNSUPPORTED;
}

/* Effects the backend may match for `op`. Block rules only match where they
 * are enforceable; on backends without the pre-op hook they still match, so
 * the caller degrades them through te_effect_mode() and reports an unblocked
 * violation. Keeping block rules out of the mask when the hook IS attached
 * prevents a denied operation from being reported twice (LSM hook with
 * blocked=1 plus tracepoint with blocked=0). */
static __always_inline unsigned int te_supported_effects(
	unsigned int backend_mode, unsigned int op, unsigned int features,
	unsigned int enforce)
{
	if (backend_mode == TE_MODE_BLOCK)
		return (1U << TEFFECT_BLOCK);
	if (te_block_hook_attached(op, features, enforce))
		return (1U << TEFFECT_NOTIFY) | (1U << TEFFECT_KILL);
	return (1U << TEFFECT_NOTIFY) | (1U << TEFFECT_KILL) |
	       (1U << TEFFECT_BLOCK);
}

/* The kernel emits exactly one kind of event: a taint-rule violation. */
enum event_type {
	EVENT_TYPE_TAINT_VIOLATION = 3,
};

struct event {
	enum event_type type;
	int pid;
	int ppid;
	unsigned int blocked;          /* 1 if an LSM hook denied the operation */
	unsigned int killed;           /* 1 if the rule sent SIGKILL */
	unsigned int effect;           /* enum taint_effect declared by the rule */
	unsigned int op;               /* enum taint_op for this matched operation */
	unsigned int domain_id;         /* runtime domain whose rule matched */
	int session_root;               /* root pid for session-scoped state */
	unsigned long long timestamp_ns;
	char comm[TASK_COMM_LEN];
	char filename[MAX_FILENAME_LEN]; /* offending exe / path ("" for connect) */
	unsigned int taint_rule_id;
	unsigned int conn_ip;            /* connect: network-order IPv4 (0 otherwise) */
	unsigned long long taint_label;
	unsigned long long matched_label;
	unsigned long long matched_labels;
	unsigned long long prov_label;
	unsigned long long prov_timestamp_ns;
	int prov_pid;
	unsigned int prov_op;            /* enum taint_op that introduced prov_label */
	unsigned int prov_ip;            /* endpoint provenance, network order */
	char prov_target[MAX_FILENAME_LEN]; /* file/exec provenance target */
};

#endif /* __PROCESS_H */
