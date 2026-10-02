/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
/* Copyright (c) 2026 eunomia-bpf org. */
#ifndef __PROCESS_H
#define __PROCESS_H

#define TASK_COMM_LEN 16
#define MAX_FILENAME_LEN 127

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
