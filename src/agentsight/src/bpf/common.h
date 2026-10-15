#ifndef COMMON_H
#define COMMON_H

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>

#ifndef RING_BUFFER_SIZE
#define RING_BUFFER_SIZE (32 * 1024 * 1024)
#endif

#ifndef MAX_TRACED_PROCESSES
#define MAX_TRACED_PROCESSES 1024
#endif


// Event source identifiers - first field of every ringbuffer event
// Allows unified dispatch from a shared ring buffer
typedef enum {
    EVENT_SOURCE_PROC = 1,   // Process events (proctrace)
    EVENT_SOURCE_SSL  = 2,   // SSL/TLS traffic events (sslsniff)
    EVENT_SOURCE_PROCMON = 3, // Process monitor events (procmon)
    EVENT_SOURCE_FILEWATCH = 4, // File watch events (filewatch)
    EVENT_SOURCE_FILEWRITE = 5, // File write events (filewrite)
    EVENT_SOURCE_UDPDNS = 6,   // UDP DNS query events (udpdns)
} event_source_t;

// Common event header - every ringbuffer event MUST start with this
// Allows user-space to read source and dispatch to the right handler
struct common_event_hdr {
    u32 source;  // event_source_t - identifies the event producer
};

// Shared ring buffer - used by all BPF programs to avoid wasting memory
struct
{
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, RING_BUFFER_SIZE);
} rb SEC(".maps");

enum internal_metric_key {
    RING_BUFFER_DROPPED = 0,
};

// Shared per-CPU counters avoid contention in probe hot paths. User space
// sums every CPU value when exporting the runtime metrics snapshot.
struct
{
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, u32);
    __type(value, u64);
} internal_metrics SEC(".maps");

static __always_inline void record_ring_buffer_drop(void)
{
    u32 key = RING_BUFFER_DROPPED;
    u64 *count = bpf_map_lookup_elem(&internal_metrics, &key);
    if (count)
        *count += 1;
}

#ifndef NO_TRACED_PROCESSES_MAP
// Shared traced_processes map - used by all BPF programs for process filtering
struct
{
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_TRACED_PROCESSES);
    __type(key, u32);
    __type(value, u32);
} traced_processes SEC(".maps");
#endif

struct pid_link
{
  struct hlist_node node;
  struct pid *pid;
};

struct task_struct___older_v50
{
  struct pid_link pids[PIDTYPE_MAX];
};


static inline u32 get_task_ns_pid(struct task_struct *task)
{
  unsigned int level = 0;
  struct pid *pid = NULL;
  /* Read from the thread-group leader so we return the PROCESS-level (tgid)
   * namespace pid, not the calling thread's ns tid. A syscall may run on a
   * worker thread (e.g. aiohttp/uvloop performs getaddrinfo in a thread pool),
   * in which case the raw current-task PIDTYPE_PID is a per-thread tid. Reading
   * the leader keeps event->pid == process pid so it correlates with sslsniff
   * and cmdline/DNS discovery. For a typical single-threaded process the
   * leader is the task itself. */
  struct task_struct *leader = BPF_CORE_READ(task, group_leader);
  /* Defensive: group_leader is never NULL in practice, but if a CO-RE read
   * ever yields 0 fall back to task so we degrade to the prior (per-thread)
   * behaviour instead of dereferencing a NULL leader. */
  if (!leader)
    leader = task;

  if (bpf_core_type_exists(struct pid_link))
  {
    struct task_struct___older_v50 *t = (void *)leader;
    pid = BPF_CORE_READ(t, pids[PIDTYPE_PID].pid);
  }
  else
  {
    pid = BPF_CORE_READ(leader, thread_pid);
  }

  level = BPF_CORE_READ(pid, level);

  return BPF_CORE_READ(pid, numbers[level].nr);
}

/* Set by user-space when it observes /proc through the initial pid namespace;
 * see probes::pidns::proc_root_is_init_pidns() for how that is determined.
 * Defaults to false so an unconfigured object keeps the historical behaviour. */
const volatile bool observer_pidns_is_init = false;

/*
 * current_observer_pid - the current process's PID *as user-space sees it*.
 *
 * Every consumer of an event pid resolves it against user-space's own /proc --
 * cmdline, maps, cgroup, and the /proc/<pid>/root uprobe paths -- and /proc is
 * rendered in the *reader's* pid namespace. So the number reported here has to
 * be the one valid in user-space's namespace.
 *
 * get_task_ns_pid() returns the target's *innermost* namespace pid. That is the
 * right answer only when observer and target share a namespace: agentsight on a
 * host, or a sidecar with shareProcessNamespace, which is why the distinction
 * went unnoticed. It is the wrong answer when user-space sits in the initial
 * namespace and the target is inside a container -- a DaemonSet with
 * hostPID:true -- because a container-local pid either does not exist on the
 * host or, worse, belongs to an unrelated process that then gets probed in its
 * place.
 *
 * For an initial-namespace observer the correct number is simply the host tgid,
 * which every task has, so no namespace walking is needed. Note that
 * bpf_get_ns_current_pid_tgid() cannot serve here: it returns -EINVAL unless the
 * namespace named by (dev, ino) is the *current task's own*, so it does not
 * translate into an ancestor namespace.
 *
 * We take the tgid rather than the tid for the same reason get_task_ns_pid()
 * reads the group leader: a syscall may run on a worker thread (e.g.
 * aiohttp/uvloop doing getaddrinfo in a thread pool), and the event pid must
 * stay process-level so it correlates with sslsniff and cmdline/DNS discovery.
 */
static __always_inline u32 current_observer_pid(void)
{
    if (observer_pidns_is_init)
        return (u32)(bpf_get_current_pid_tgid() >> 32);

    return get_task_ns_pid((struct task_struct *)bpf_get_current_task());
}

/* Convenience wrapper: get the observer-namespace PID of the current task.
 * In non-container scenarios this equals bpf_get_current_pid_tgid() >> 32. */
static __always_inline u32 current_ns_pid(void)
{
    return current_observer_pid();
}

/*
 * is_pid_traced - check whether the current process should be traced.
 *
 * Returns the PID to use for event->pid if the process is traced, or 0 if it
 * should be skipped. Checks both the host PID and the observer-namespace PID,
 * so user-space may register either and still get a match.
 *
 * The returned pid is deliberately whichever key hit the map: that keeps
 * event->pid equal to the pid user-space registered, which is what lets
 * ConnectionId(pid, ssl_ptr) correlate with the discovery-side caches.
 */
#ifndef NO_TRACED_PROCESSES_MAP
static __always_inline u32 is_pid_traced(u32 host_pid)
{
    u32 *traced = bpf_map_lookup_elem(&traced_processes, &host_pid);
    if (traced)
        return host_pid;

    /* Container scenario: host PID != observer-namespace PID.
     * Resolve the pid as user-space sees it and retry the lookup. */
    u32 observer_pid = current_observer_pid();

    if (observer_pid != host_pid) {
        traced = bpf_map_lookup_elem(&traced_processes, &observer_pid);
        if (traced)
            return observer_pid;
    }

    return 0;
}
#endif

/* ========== cgroup filter ==========
 *
 * Optional cgroup-level filter shared across all probes that include common.h.
 * When `filter_cgroup_enabled` is false (default), the cgroup filter logic in
 * each probe short-circuits to true and behavior is identical to before.
 * When enabled, only cgroups registered in the cgroup_filter map pass.
 *
 * Probes that act as full-system audit (e.g. procmon) should define
 * NO_CGROUP_FILTER before including common.h to opt out entirely.
 */
#ifndef NO_CGROUP_FILTER
#ifndef MAX_CGROUP_FILTER_ENTRIES
#define MAX_CGROUP_FILTER_ENTRIES 512
#endif

const volatile bool filter_cgroup_enabled = false;

struct
{
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_CGROUP_FILTER_ENTRIES);
    __type(key, u64);    /* cgroup inode id from get_cgroup_id_compat() */
    __type(value, u8);   /* 1 = tracked */
} cgroup_filter SEC(".maps");
#endif

#endif
