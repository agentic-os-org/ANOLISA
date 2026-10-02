// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
/* Copyright (c) 2026 eunomia-bpf org. */
//
// ActPlane taint loader. Reads a compiled policy (struct taint_config, produced
// by the collector's DSL compiler) into the BPF rodata, attaches the enforcer,
// and prints the TAINT_VIOLATION events the kernel emits.

#include <argp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <stdlib.h>
#include <errno.h>
#include <stdbool.h>
#include <sys/types.h>
#include <unistd.h>
#include <bpf/bpf.h>
#include <bpf/libbpf.h>
#include "process.h"
#include "process.skel.h"

static struct env {
	bool verbose;
	const char *config;
	pid_t seed_pid;
	unsigned long long seed_label;
} env = {0};

struct cap_policy_mask {
	unsigned long long lo;
	unsigned long long hi;
};

const char *argp_program_version = "actplane-taint 2.0";
const char argp_program_doc[] =
	"ActPlane in-kernel taint enforcer.\n"
	"\n"
	"Loads a compiled policy and reports only taint-rule violations.\n"
	"USAGE: ./process --config policy.bin [--seed-pid PID [--seed-label BIT]]\n";

static const struct argp_option opts[] = {
	{ "config", 'c', "FILE", 0, "Compiled policy (struct taint_config blob)" },
	{ "seed-pid", 1000, "PID", 0, "Bind this pid as the active root process" },
	{ "seed-label", 1001, "BIT", 0, "Label bit to apply to --seed-pid" },
	{ "verbose", 'v', NULL, 0, "Verbose libbpf debug output" },
	{},
};

static error_t parse_arg(int key, char *arg, struct argp_state *state)
{
	switch (key) {
	case 'v': env.verbose = true; break;
	case 'c': env.config = arg; break;
	case 1000: env.seed_pid = (pid_t)strtol(arg, NULL, 10); break;
	case 1001: env.seed_label = strtoull(arg, NULL, 0); break;
	case ARGP_KEY_ARG: argp_usage(state); break;
	default: return ARGP_ERR_UNKNOWN;
	}
	return 0;
}
static const struct argp argp = { .options = opts, .parser = parse_arg, .doc = argp_program_doc };

static int libbpf_print_fn(enum libbpf_print_level level, const char *format, va_list args)
{
	if (level == LIBBPF_DEBUG && !env.verbose)
		return 0;
	return vfprintf(stderr, format, args);
}

static volatile bool exiting = false;
static void sig_handler(int sig) { (void)sig; exiting = true; }

static bool bpf_lsm_active(void)
{
	FILE *f = fopen("/sys/kernel/security/lsm", "r");
	char buf[512];
	bool active = false;

	if (!f)
		return false;
	if (fgets(buf, sizeof(buf), f))
		active = strstr(buf, "bpf") != NULL;
	fclose(f);
	return active;
}

static const char *effect_name(unsigned int effect)
{
	switch (effect) {
	case TEFFECT_NOTIFY: return "notify";
	case TEFFECT_KILL: return "kill";
	case TEFFECT_BLOCK: return "block";
	default: return "unknown";
	}
}

static bool config_has_effect(const struct taint_config *cfg, unsigned int effect)
{
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].effect == effect)
			return true;
	}
	return false;
}

static bool config_has_block_op(const struct taint_config *cfg, unsigned int op)
{
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].effect != TEFFECT_BLOCK || cfg->rules[i].op != op)
			continue;
		if (op == TOP_EXEC && cfg->rules[i].arg[0] != '\0')
			continue;
		return true;
	}
	return false;
}

static bool config_has_argv_block_exec(const struct taint_config *cfg)
{
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].effect == TEFFECT_BLOCK &&
		    cfg->rules[i].op == TOP_EXEC &&
		    cfg->rules[i].arg[0] != '\0')
			return true;
	}
	return false;
}

static bool config_has_op(const struct taint_config *cfg, unsigned int op)
{
	for (unsigned int i = 0; i < cfg->n_updates && i < MAX_TAINT_UPDATES; i++) {
		if (cfg->updates[i].op == op)
			return true;
	}
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].op == op)
			return true;
	}
	return false;
}

static unsigned int path_match_features(unsigned int match)
{
	switch (match) {
	case TAINT_MATCH_CONTAINS: return TE_POLICY_PATH_CONTAINS;
	case TAINT_MATCH_SUFFIX: return TE_POLICY_PATH_SUFFIX;
	default: return 0;
	}
}

static unsigned int config_features(const struct taint_config *cfg)
{
	unsigned int features = 0;

	for (unsigned int i = 0; i < cfg->n_updates && i < MAX_TAINT_UPDATES; i++) {
		if (cfg->updates[i].op == TOP_OPEN || cfg->updates[i].op == TOP_WRITE)
			features |= TE_POLICY_FILE_FLOW |
				    path_match_features(cfg->updates[i].match);
		if (cfg->updates[i].op == TOP_CONNECT)
			features |= TE_POLICY_CONNECT;
		if (cfg->updates[i].op == TOP_RECV)
			features |= TE_POLICY_RECV;
	}
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].effect == TEFFECT_BLOCK) {
			if (cfg->rules[i].op == TOP_EXEC && cfg->rules[i].arg[0] == '\0')
				features |= TE_POLICY_BLOCK_EXEC;
			if (cfg->rules[i].op == TOP_OPEN ||
			    cfg->rules[i].op == TOP_WRITE)
				features |= TE_POLICY_BLOCK_FILE;
			if (cfg->rules[i].op == TOP_CONNECT)
				features |= TE_POLICY_BLOCK_CONNECT;
		}
		if (cfg->rules[i].op == TOP_OPEN) {
			features |= TE_POLICY_OPEN_RULES |
				    path_match_features(cfg->rules[i].match);
			if (cfg->rules[i].cond_kind == TCOND_TARGET)
				features |= path_match_features(cfg->rules[i].cond_match);
		}
		if (cfg->rules[i].op == TOP_WRITE) {
			features |= TE_POLICY_FILE_FLOW |
				    TE_POLICY_WRITE_RULES |
				    path_match_features(cfg->rules[i].match);
			if (cfg->rules[i].cond_kind == TCOND_TARGET)
				features |= path_match_features(cfg->rules[i].cond_match);
		}
		if (cfg->rules[i].op == TOP_CONNECT)
			features |= TE_POLICY_CONNECT;
		if (cfg->rules[i].op == TOP_RECV)
			features |= TE_POLICY_RECV;
	}
	return features;
}

static bool config_has_file_write(const struct taint_config *cfg)
{
	for (unsigned int i = 0; i < cfg->n_updates && i < MAX_TAINT_UPDATES; i++) {
		if (cfg->updates[i].op == TOP_WRITE)
			return true;
	}
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].op == TOP_WRITE)
			return true;
	}
	return false;
}

static bool env_flag(const char *name)
{
	const char *v = getenv(name);
	return v && v[0] != '\0' && strcmp(v, "0") != 0;
}

static bool hook_profile_full(void)
{
	const char *v = getenv("ACTPLANE_HOOK_PROFILE");
	if (!v)
		return false;
	return strcasecmp(v, "full") == 0 ||
	       strcasecmp(v, "all") == 0 ||
	       strcasecmp(v, "wide") == 0;
}

static unsigned int all_hook_features(void)
{
	return TE_POLICY_CONNECT |
	       TE_POLICY_RECV |
	       TE_POLICY_FILE_FLOW |
	       TE_POLICY_BLOCK_EXEC |
	       TE_POLICY_BLOCK_FILE |
	       TE_POLICY_BLOCK_CONNECT;
}

static bool name_in(const char *name, const char *const *items, size_t n)
{
	for (size_t i = 0; i < n; i++) {
		if (strcmp(name, items[i]) == 0)
			return true;
	}
	return false;
}

static int tracepoint_autoload_needed(const char *name, unsigned int features,
				      bool file_write, bool advanced)
{
	static const char *const core[] = {
		"handle_fork", "handle_exit", "cap_drain_tick",
	};
	static const char *const file_open[] = {
		"trace_openat", "trace_openat_exit", "trace_open",
		"trace_open_exit", "trace_openat2", "trace_openat2_exit",
	};
	static const char *const file_write_path[] = {
		"trace_creat", "trace_creat_exit", "trace_truncate",
		"trace_truncate_exit", "trace_unlink", "trace_unlinkat",
		"trace_rename", "trace_rename_exit", "trace_renameat",
		"trace_renameat_exit", "trace_renameat2",
		"trace_renameat2_exit",
	};
	static const char *const fd_flow[] = {
		"trace_read", "trace_read_exit", "trace_write",
		"trace_write_exit", "trace_close", "trace_dup",
		"trace_dup_exit", "trace_dup2", "trace_dup2_exit",
		"trace_dup3", "trace_dup3_exit", "trace_fcntl",
		"trace_fcntl_exit",
	};
	static const char *const connect_or_recv[] = {
		"trace_connect", "trace_connect_exit",
	};
	static const char *const send_addr[] = {
		"trace_sendto", "trace_sendto_exit", "trace_sendmsg",
		"trace_sendmsg_exit",
	};
	static const char *const recv_addr[] = {
		"trace_recvfrom", "trace_recvfrom_exit", "trace_recvmsg",
		"trace_recvmsg_exit",
	};
	static const char *const file_advanced[] = {
		"trace_pipe", "trace_pipe_exit", "trace_pipe2",
		"trace_pipe2_exit", "trace_socketpair",
		"trace_socketpair_exit", "trace_bind", "trace_bind_exit",
		"trace_accept", "trace_accept_exit", "trace_accept4",
		"trace_accept4_exit", "trace_sendfile64",
		"trace_sendfile64_exit", "trace_copy_file_range",
		"trace_copy_file_range_exit", "trace_splice",
		"trace_splice_exit", "trace_mmap", "trace_mmap_exit",
		"trace_mprotect", "trace_mprotect_exit", "trace_mremap",
		"trace_mremap_exit", "trace_munmap", "trace_munmap_exit",
	};
	bool file_flow = features & TE_POLICY_FILE_FLOW;
	bool open_rules = features & TE_POLICY_OPEN_RULES;
	bool connect = features & TE_POLICY_CONNECT;
	bool recv = features & TE_POLICY_RECV;

	if (name_in(name, core, sizeof(core) / sizeof(core[0])))
		return 1;
	if (strcmp(name, "handle_exec") == 0)
		return 0;
	if (strcmp(name, "handle_exec_args") == 0)
		return 1;
	if (name_in(name, file_open, sizeof(file_open) / sizeof(file_open[0])))
		return file_flow || open_rules;
	if (name_in(name, file_write_path,
		    sizeof(file_write_path) / sizeof(file_write_path[0])))
		return file_write || file_flow;
	if (name_in(name, fd_flow, sizeof(fd_flow) / sizeof(fd_flow[0])))
		return file_flow || connect || recv;
	if (name_in(name, connect_or_recv,
		    sizeof(connect_or_recv) / sizeof(connect_or_recv[0])))
		return connect || recv || (file_flow && advanced);
	if (name_in(name, send_addr, sizeof(send_addr) / sizeof(send_addr[0])))
		return connect || (file_flow && advanced);
	if (name_in(name, recv_addr, sizeof(recv_addr) / sizeof(recv_addr[0])))
		return recv || (file_flow && advanced);
	if (name_in(name, file_advanced,
		    sizeof(file_advanced) / sizeof(file_advanced[0])))
		return file_flow && advanced;
	return -1;
}

static void configure_tracepoint_autoload(struct process_bpf *skel,
					  unsigned int features,
					  bool file_write, bool advanced)
{
	struct bpf_program *prog;

	bpf_object__for_each_program(prog, skel->obj) {
		const char *name = bpf_program__name(prog);
		int needed = tracepoint_autoload_needed(name, features,
							file_write, advanced);
		if (needed >= 0)
			bpf_program__set_autoload(prog, needed);
	}
}

static void configure_exec_tail_programs(struct process_bpf *skel)
{
	bpf_program__set_autoattach(skel->progs.exec_tp_update_simple, false);
	bpf_program__set_autoattach(skel->progs.exec_tp_update_prefix, false);
	bpf_program__set_autoattach(skel->progs.exec_tp_rule_simple, false);
	bpf_program__set_autoattach(skel->progs.exec_tp_rule_complex, false);
}

static int install_exec_tail_programs(struct process_bpf *skel)
{
	struct {
		__u32 idx;
		struct bpf_program *prog;
	} entries[] = {
		{ 0, skel->progs.exec_tp_update_simple },
		{ 1, skel->progs.exec_tp_update_prefix },
		{ 2, skel->progs.exec_tp_rule_simple },
		{ 3, skel->progs.exec_tp_rule_complex },
	};
	int map_fd = bpf_map__fd(skel->maps.exec_tail);

	for (size_t i = 0; i < sizeof(entries) / sizeof(entries[0]); i++) {
		int prog_fd = bpf_program__fd(entries[i].prog);
		if (prog_fd < 0 ||
		    bpf_map_update_elem(map_fd, &entries[i].idx, &prog_fd, BPF_ANY) < 0) {
			fprintf(stderr, "failed to install exec tail program %u: %s\n",
				entries[i].idx, strerror(errno));
			return -1;
		}
	}
	return 0;
}

static int validate_config(const struct taint_config *cfg)
{
	for (unsigned int i = 0; i < cfg->n_updates && i < MAX_TAINT_UPDATES; i++) {
		if (cfg->updates[i].op == TOP_EXEC &&
		    cfg->updates[i].match == TAINT_MATCH_SUFFIX) {
			fprintf(stderr,
				"config update[%u]: suffix exec matches are unsupported; use DSL exec patterns that lower to exact/prefix\n",
				i);
			return -1;
		}
	}
	for (unsigned int i = 0; i < cfg->n_rules && i < MAX_TAINT_RULES; i++) {
		if (cfg->rules[i].op == TOP_EXEC &&
		    cfg->rules[i].match == TAINT_MATCH_SUFFIX) {
			fprintf(stderr,
				"config rule[%u]: suffix exec matches are unsupported; use DSL exec patterns that lower to exact/prefix\n",
				i);
			return -1;
		}
		if (cfg->rules[i].op == TOP_EXEC &&
		    cfg->rules[i].cond_kind == TCOND_TARGET &&
		    cfg->rules[i].cond_match == TAINT_MATCH_SUFFIX) {
			fprintf(stderr,
				"config rule[%u]: suffix exec target conditions are unsupported; use exact/prefix exec patterns\n",
				i);
			return -1;
		}
	}
	return 0;
}

#define JSON_ESC_BUFSZ (MAX_FILENAME_LEN * 6 + 3)

static void json_escape(char *out, size_t out_sz, const char *in)
{
	size_t pos = 0;

	if (!out_sz)
		return;
	out[pos++] = '"';
	for (size_t i = 0; in && in[i] && pos + 2 < out_sz; i++) {
		unsigned char c = (unsigned char)in[i];

		if (c == '"' || c == '\\') {
			if (pos + 2 >= out_sz)
				break;
			out[pos++] = '\\';
			out[pos++] = c;
		} else if (c == '\n' || c == '\r' || c == '\t' ||
			   c == '\b' || c == '\f') {
			if (pos + 2 >= out_sz)
				break;
			out[pos++] = '\\';
			out[pos++] = c == '\n' ? 'n' :
				     c == '\r' ? 'r' :
				     c == '\t' ? 't' :
				     c == '\b' ? 'b' : 'f';
		} else if (c < 0x20 || c >= 0x80) {
			if (pos + 6 >= out_sz)
				break;
			pos += snprintf(out + pos, out_sz - pos, "\\u%04x", c);
		} else {
			out[pos++] = c;
		}
	}
	if (pos + 1 >= out_sz)
		pos = out_sz - 2;
	out[pos++] = '"';
	out[pos] = '\0';
}

static int handle_event(void *ctx, void *data, size_t sz)
{
	const struct event *e = data;
	char target[MAX_FILENAME_LEN];
	char target_json[JSON_ESC_BUFSZ];
	char prov_target_json[JSON_ESC_BUFSZ];
	char comm_json[TASK_COMM_LEN * 6 + 3];
	char prov_json[JSON_ESC_BUFSZ + 192];
	(void)ctx; (void)sz;
	if (e->type != EVENT_TYPE_TAINT_VIOLATION)
		return 0;
	if (e->conn_ip) { /* connect: format the network-order IPv4 */
		unsigned int ip = e->conn_ip;
		snprintf(target, sizeof(target), "%u.%u.%u.%u",
			 ip & 0xff, (ip >> 8) & 0xff, (ip >> 16) & 0xff, (ip >> 24) & 0xff);
	} else {
		snprintf(target, sizeof(target), "%s", e->filename);
	}
	json_escape(target_json, sizeof(target_json), target);
	json_escape(prov_target_json, sizeof(prov_target_json), e->prov_target);
	json_escape(comm_json, sizeof(comm_json), e->comm);
	if (e->prov_label) {
		snprintf(prov_json, sizeof(prov_json),
			 "{\"label\":%llu,\"timestamp_ns\":%llu,"
			 "\"pid\":%d,\"op\":%u,\"ip\":%u,\"target\":%s}",
			 e->prov_label, e->prov_timestamp_ns, e->prov_pid,
			 e->prov_op, e->prov_ip, prov_target_json);
	} else {
		snprintf(prov_json, sizeof(prov_json), "null");
	}
	printf("{\"timestamp\":%llu,\"event\":\"TAINT_VIOLATION\",\"effect\":\"%s\","
	       "\"blocked\":%s,\"killed\":%s,\"comm\":%s,\"pid\":%d,\"ppid\":%d,"
	       "\"op\":%u,\"domain_id\":%u,\"session_root\":%d,"
	       "\"target\":%s,\"rule_id\":%u,\"taint_label\":%llu,"
	       "\"matched_label\":%llu,\"matched_labels\":%llu,"
	       "\"provenance\":%s}\n",
	       e->timestamp_ns, effect_name(e->effect), e->blocked ? "true" : "false",
	       e->killed ? "true" : "false", comm_json, e->pid, e->ppid,
	       e->op, e->domain_id, e->session_root,
	       target_json, e->taint_rule_id, e->taint_label, e->matched_label,
	       e->matched_labels, prov_json);
	fflush(stdout);
	return 0;
}

static int load_config(const char *path, struct taint_config *cfg)
{
	FILE *f = fopen(path, "rb");
	if (!f) {
		fprintf(stderr, "cannot open config '%s': %s\n", path, strerror(errno));
		return -1;
	}
	memset(cfg, 0, sizeof(*cfg));
	size_t n = fread(cfg, 1, sizeof(*cfg), f);
	fclose(f);
	if (n != sizeof(*cfg)) {
		fprintf(stderr, "config size mismatch: read %zu, expected %zu\n",
			n, sizeof(*cfg));
		return -1;
	}
	return 0;
}

struct proc_state_seed {
	unsigned long long labels;
	unsigned long long lin_gates;
};

struct cap_state_seed {
	unsigned int parent;
	unsigned int scope_id;
	unsigned long long labels;
	unsigned long long authority_mask;
	unsigned long long target_mask;
	unsigned long long restrict_mask;
	unsigned long long gate_mask;
	unsigned long long label_mask;
};

static int seed_initial_label(struct process_bpf *skel)
{
	struct proc_state_seed state = {};
	struct cap_state_seed cap = {};
	pid_t pid = env.seed_pid;
	unsigned int target_id;

	if (!env.seed_pid && !env.seed_label)
		return 0;
	if (env.seed_pid <= 0) {
		fprintf(stderr, "--seed-label requires --seed-pid, and --seed-pid must be positive\n");
		return -1;
	}

	state.labels = env.seed_label;
	if (bpf_map_update_elem(bpf_map__fd(skel->maps.ts_proc), &pid, &state, BPF_ANY) < 0) {
		fprintf(stderr, "failed to seed pid %d in ts_proc: %s\n", pid, strerror(errno));
		return -1;
	}
	if (bpf_map_update_elem(bpf_map__fd(skel->maps.ts_root), &pid, &pid, BPF_ANY) < 0) {
		fprintf(stderr, "failed to seed pid %d in ts_root: %s\n", pid, strerror(errno));
		return -1;
	}
	target_id = (unsigned int)pid;
	cap.scope_id = 1;
	cap.labels = env.seed_label;
	cap.authority_mask = (1ULL << 4) | (1ULL << 3); /* bind_rule | narrow_scope */
	cap.target_mask = (1ULL << 0) | (1ULL << 1);    /* self | child */
	if (bpf_map_update_elem(bpf_map__fd(skel->maps.cap_task), &pid, &target_id,
				BPF_ANY) < 0) {
		fprintf(stderr, "failed to bind pid %d in cap_task: %s\n", pid, strerror(errno));
		return -1;
	}
	if (bpf_map_update_elem(bpf_map__fd(skel->maps.cap_state), &target_id, &cap,
				BPF_ANY) < 0) {
		fprintf(stderr, "failed to seed target %u in cap_state: %s\n",
			target_id, strerror(errno));
		return -1;
	}
	/* ts_sess (gate/staleness state) is created lazily by te_sess_init when the
	 * first gate or `since` invalidator fires; an absent entry reads as all-zero
	 * (no gate fired yet), which is the correct initial state. */
	fprintf(stderr, "ActPlane: seeded pid %d label 0x%llx\n", pid, env.seed_label);
	return 0;
}

static int protect_loader_pid(struct process_bpf *skel)
{
	pid_t pid = getpid();
	__u32 one = 1;

	if (bpf_map_update_elem(bpf_map__fd(skel->maps.te_protected_pids),
				&pid, &one, BPF_ANY) < 0) {
		fprintf(stderr, "failed to protect loader pid %d: %s\n",
			pid, strerror(errno));
		return -1;
	}
	return 0;
}

int main(int argc, char **argv)
{
	struct ring_buffer *rb = NULL;
	struct process_bpf *skel;
	struct taint_config cfg;
	bool enforce;
	int err;

	err = argp_parse(&argp, argc, argv, 0, NULL, NULL);
	if (err)
		return err;
	if (!env.config) {
		fprintf(stderr, "missing --config <policy.bin>\n");
		return 1;
	}
	if (load_config(env.config, &cfg))
		return 1;
	if (validate_config(&cfg))
		return 1;
	enforce = bpf_lsm_active();

	libbpf_set_print(libbpf_print_fn);
	signal(SIGINT, sig_handler);
	signal(SIGTERM, sig_handler);

	skel = process_bpf__open();
	if (!skel) {
		fprintf(stderr, "Failed to open BPF skeleton\n");
		return 1;
	}
	bool block_exec = config_has_block_op(&cfg, TOP_EXEC);
	bool block_file = config_has_block_op(&cfg, TOP_OPEN) ||
			  config_has_block_op(&cfg, TOP_WRITE);
	bool file_write = config_has_file_write(&cfg);
	bool block_connect = config_has_block_op(&cfg, TOP_CONNECT);
	bool recv_flow = config_has_op(&cfg, TOP_RECV);
	bool full_profile = hook_profile_full();
	bool advanced_hooks = full_profile ||
			      env_flag("ACTPLANE_ENABLE_ADVANCED_HOOKS") ||
			      env_flag("ACTPLANE_ADVANCED_TRACEPOINTS");
	unsigned int features = full_profile ? config_features(&cfg) | all_hook_features() :
					       config_features(&cfg);
	if (env_flag("ACTPLANE_RESERVE_FILE_FLOW"))
		features |= TE_POLICY_FILE_FLOW;
	if (full_profile) {
		block_exec = true;
		block_file = true;
		file_write = true;
		block_connect = true;
		recv_flow = true;
	}
	configure_tracepoint_autoload(skel, features, file_write, advanced_hooks);
	configure_exec_tail_programs(skel);
	if (!enforce || !block_exec)
		bpf_program__set_autoload(skel->progs.enforce_bprm_check_security, false);
	if (!enforce || !block_file) {
		bpf_program__set_autoload(skel->progs.enforce_file_permission, false);
		bpf_program__set_autoload(skel->progs.enforce_mmap_file, false);
		bpf_program__set_autoload(skel->progs.enforce_file_mprotect, false);
	} else if (!advanced_hooks) {
		bpf_program__set_autoload(skel->progs.enforce_mmap_file, false);
		bpf_program__set_autoload(skel->progs.enforce_file_mprotect, false);
	}
	if (!enforce || !block_file) {
		bpf_program__set_autoload(skel->progs.enforce_file_open, false);
		bpf_program__set_autoload(skel->progs.enforce_file_truncate, false);
		bpf_program__set_autoload(skel->progs.enforce_path_truncate, false);
		bpf_program__set_autoload(skel->progs.enforce_path_unlink, false);
		bpf_program__set_autoload(skel->progs.enforce_path_rename, false);
	}
	if (!enforce || !block_connect)
		bpf_program__set_autoload(skel->progs.enforce_socket_connect, false);
	if (!enforce || !recv_flow)
		bpf_program__set_autoload(skel->progs.enforce_socket_recvmsg, false);
	if (!enforce) {
		bpf_program__set_autoload(skel->progs.enforce_task_kill, false);
		bpf_program__set_autoload(skel->progs.enforce_ptrace_access_check, false);
		bpf_program__set_autoload(skel->progs.enforce_bpf_syscall, false);
	}

	/* install enforce_mode into rodata before load */
	skel->rodata->enforce_mode = enforce ? 1 : 0;
	skel->rodata->policy_features = features;
	fprintf(stderr, "ActPlane: %u updates, %u rules\n",
		cfg.n_updates, cfg.n_rules);
	fprintf(stderr, "ActPlane: %s mode (%s)\n",
		enforce ? "enforce" : "tracepoint",
		enforce ? "BPF LSM is active" :
			  "BPF LSM is not active; block effects are unsupported, notify reports, kill terminates");
	if (!enforce && config_has_effect(&cfg, TEFFECT_BLOCK))
		fprintf(stderr,
			"ActPlane: warning: effect block requires BPF LSM; block rules will not fire in tracepoint mode\n");
	if (config_has_argv_block_exec(&cfg))
		fprintf(stderr,
			"ActPlane: warning: argv-sensitive block exec rules cannot block pre-exec; use kill exec for argv-token enforcement\n");

	err = process_bpf__load(skel);
	if (err) { fprintf(stderr, "Failed to load BPF skeleton\n"); goto cleanup; }
	if (install_exec_tail_programs(skel)) { err = -1; goto cleanup; }

	/* Populate writable array maps for updates and rules. */
	{
		int ufd = bpf_map__fd(skel->maps.ts_updates);
		for (__u32 i = 0; i < cfg.n_updates; i++) {
			if (bpf_map_update_elem(ufd, &i, &cfg.updates[i], BPF_ANY) < 0) {
				fprintf(stderr, "failed to set update %u: %s\n", i, strerror(errno));
				err = -1; goto cleanup;
			}
		}
		int rfd = bpf_map__fd(skel->maps.ts_rules);
		for (__u32 i = 0; i < cfg.n_rules; i++) {
			if (bpf_map_update_elem(rfd, &i, &cfg.rules[i], BPF_ANY) < 0) {
				fprintf(stderr, "failed to set rule %u: %s\n", i, strerror(errno));
				err = -1; goto cleanup;
			}
		}
		int pfd = bpf_map__fd(skel->maps.cap_policy);
		for (__u32 i = 0; i < cfg.n_rules; i++) {
			__u32 domain_id = cfg.rules[i].domain_id;
			struct cap_policy_mask mask = {};
			if (bpf_map_lookup_elem(pfd, &domain_id, &mask) < 0 && errno != ENOENT) {
				fprintf(stderr, "failed to read policy mask %u: %s\n",
					domain_id, strerror(errno));
				err = -1; goto cleanup;
			}
			if (i < 64)
				mask.lo |= 1ULL << i;
			else
				mask.hi |= 1ULL << (i - 64);
			if (bpf_map_update_elem(pfd, &domain_id, &mask, BPF_ANY) < 0) {
				fprintf(stderr, "failed to set policy mask %u: %s\n",
					domain_id, strerror(errno));
				err = -1; goto cleanup;
			}
		}
	}

	/* Loop counts in a (non-frozen) map so the verifier checks each bpf_loop
	 * callback once, not once per table entry. Slots: 0=rules 1=updates
	 * 5=labels. */
	{
		__u32 ks[6] = {0, 1, 2, 3, 4, 5};
		__u32 vs[6] = { cfg.n_rules, cfg.n_updates, 0, 0, 0,
				MAX_TAINT_LABELS };
		int cfd = bpf_map__fd(skel->maps.ts_counts);
		for (int i = 0; i < 6; i++) {
			if (bpf_map_update_elem(cfd, &ks[i], &vs[i], BPF_ANY) < 0) {
				fprintf(stderr, "failed to set loop count %d: %s\n", i, strerror(errno));
				err = -1; goto cleanup;
			}
		}
	}

	if (protect_loader_pid(skel)) { err = -1; goto cleanup; }

	err = process_bpf__attach(skel);
	if (err) { fprintf(stderr, "Failed to attach BPF skeleton\n"); goto cleanup; }
	if (seed_initial_label(skel)) { err = -1; goto cleanup; }

	rb = ring_buffer__new(bpf_map__fd(skel->maps.rb), handle_event, NULL, NULL);
	if (!rb) { err = -1; fprintf(stderr, "ring buffer failed\n"); goto cleanup; }
	fprintf(stderr, "ActPlane: ready\n");

	while (!exiting) {
		err = ring_buffer__poll(rb, 100);
		if (err == -EINTR) { err = 0; break; }
		if (err < 0) { fprintf(stderr, "poll error: %d\n", err); break; }
	}

cleanup:
	ring_buffer__free(rb);
	process_bpf__destroy(skel);
	return err < 0 ? -err : 0;
}
