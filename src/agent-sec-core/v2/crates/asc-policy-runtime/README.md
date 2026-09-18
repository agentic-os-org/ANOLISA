# Policy reconciliation runtime

`src/reconciliation/` owns the Binding work queue, synchronous worker threads,
retry timer and compensation scan. It depends on generic core/repository ports;
concrete Adapter/Client construction belongs to the daemon.

`ready` is FIFO; `entries` tracks Queued, Running(dirty), WaitingRetry(deadline),
or Exhausted, with an automatic retry count carried across scheduling transitions.
Notifications carry only Binding IDs. Running notifications set dirty; waiting
notifications immediately requeue, allowing Delete to preempt an Apply retry.
Every invocation reads current intent and starts from scratch. Queue locks never
span Repository/Client I/O or join. The Running entry provides same-Binding exclusion.

Defaults: 4 workers, 65,536 total entries, 128 candidates per scan page, 100 ms timer
and scan interval, and 1 second retry for Superseded and repository errors.
Every automatic retry waits, including a RetryAt whose deadline passed during
bookkeeping (at least 1 ms). `max_auto_retries` defaults to 4 retries after the
first call; RetryAt, Superseded and repository errors share this queue budget.
`start(repository, reconciler, config)` obtains the core's shared clock through
`ReconcileAttempt::clock()`; it accepts no independent timer clock. Wrappers must
delegate that method to their core. Deadline creation, worker scheduling and scans
therefore use the same clock instance in the production composition.
Skipped only restores an existing deadline and does not consume another retry.
On automatic reschedule exhaustion, the worker retains Running, conditionally
writes APPLY_FAILED/DELETE_FAILED with RECONCILE_RETRY_EXHAUSTED, then releases
the entry and its schedule. Confirmed terminal/missing records are also released.
Only unconfirmed termination retains Exhausted so timers/scans cannot restart it.
Notifications reset the queue budget and queue waiting/exhausted work; dirty
notifications take precedence. They do not reset the business attempt progress
for the same pending request. Capacity counts running, waiting and unconfirmed
exhausted entries. Terminalization patches never change spec or deployment records.
Both queue reschedule budget and business AttemptSchedule are process-local and
do not survive Runtime recreation. Existing entries retain attempt count and
monotonic deadline through scans and duplicate wakeups. A new explicit Pending
request with no error resets its business progress on the next call.
Overflow increments a counter; continuous stable-ID scan rounds
repair missed notifications without clearing or dirtying existing entries. The
scanner resumes its cursor after a full page and wraps after the final page.
Pages include terminal Binding metadata, which the scheduler skips; inactive
records therefore cannot cause an unbounded repository scan under one lock.
`discover_many` merges each page under one queue lock and batches worker wake-ups.
Ticks still scan all entries; no deadline index or full-capacity latency guarantee
is provided. Add an index only if load measurements justify its maintenance cost.

PAP uses `BindingReconcileEnqueuer` after saving pending intent. On Full/Stopped,
PAP conditionally writes Failed and status.error, then returns the complete BindingView
in the same result envelope as accepted intent, including the saved identity. If a worker already claimed the request, PAP returns
the current record instead. Unconfirmed termination returns internal. Scans do not
restart Failed requests; callers must explicitly retry them. This admission failure
is distinct from automatic reschedule exhaustion above. Policy/Scope CRUD never
checks reconciliation readiness. Binding admission also rejects an unavailable,
stopped or fatally failed Runtime before saving with protocol `unavailable` and no
BindingView. Readiness does not check capacity; storage failures remain `internal`.
Individual attempt errors and temporary scan
failures do not close admission. Repository operations report their own errors.
A failed attempt waits in WaitingRetry; after the queue budget runs out, the worker
persists Failed before releasing its slot (Exhausted only if unconfirmed); there is no separate error-ID
set. Storage/invalid-write errors emit an ID and a payload-free error diagnostic,
since the error may prevent recording it in the Binding. CAS exhaustion returns
`StoreError::Contended`, not `Unavailable`. The next call reloads repository facts;
neither error proves remote failure or permits discarding target responsibility.
Each worker catches attempt and scheduling-read panics after core bookkeeping
unwinds, then retains Running while checking the stored outcome. Pending/running
records for the original revision and Apply/Delete intent are conditionally failed
with RECONCILE_WORKER_PANICKED. Confirmed terminal/missing records release the entry;
unconfirmed reads/writes/panics retain Exhausted and are not automatically replayed.
New intent and dirty notifications take precedence; CAS conflicts never retry the
old failure patch against a fresh snapshot. Dirty notifications still take precedence. The same
worker continues with other Bindings; successful results and cleanup responsibility
are preserved. This does not isolate aborts or repair poisoned shared dependencies.
Scan failures still degrade health until scanning succeeds. Timer/scanner panics
or panics in internal queue bookkeeping mark the service failed and stop pickup.
Runtime startup failure uses an explicit unavailable
Binding admission port. A failed Runtime is not automatically restarted. Queries,
Policy/Scope writes and other daemon services remain available.

Stop request admission and drain in-flight requests before `shutdown`. It stops
pickup/scanning and joins every actual call. The daemon supplies a 30 second outer
drain bound; timing out the waiter does not cancel a blocking call or free its
Binding ownership. Process exit remains the final cutoff. Dropping the runtime
also stops and joins its owned threads.

From `v2`:

```sh
cargo test -p asc-policy-runtime -p asc-pcp --locked --offline
```

Tests cover notification/finish and take races, deadline invalidation, overflow
and paging, single-worker retry fairness, PAP commit-before-notify, fresh input
on Update, Delete during Apply and retry waiting, health, and owned shutdown.
Core fixtures assert complete state and target call traces. The full real-process
CLI/daemon E2E remains separate; persistent restart and systematic error injection
wait for the SQL repository. No generic scheduler, durable queue or recovery is
claimed here.

One Runtime WorkQueue owns scheduling for each Repository. Its Running entry
excludes other attempts for the same Binding through Client I/O, result writes
and panic bookkeeping. The core has no second execution mutex; do not invoke it
concurrently outside this queue or through another Runtime over the same store.
