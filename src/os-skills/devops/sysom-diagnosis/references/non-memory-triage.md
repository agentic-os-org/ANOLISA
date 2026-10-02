# Non-Memory Triage

Use this reference when the user symptom is IO, load/CPU scheduling, network, or
Java memory rather than a generic memory fault.

## IO

Start with `sysom-osops io iofsstat` when the user reports high iowait, slow
disk, blocked IO, or device saturation. Continue to `sysom-osops io iodiagnose`
only when the overview points to slow IO and the answer still lacks the device,
latency source, affected process/workload, or safe next action.

Answer from the envelope once it names the affected device, severity, likely
mechanism, and remediation or next safe check.

## Load and CPU Scheduling

Use `sysom-osops load loadtask` for load composition, runqueue, or broad system
load symptoms. Use `sysom-osops load delay` when the symptom is runnable tasks
not getting CPU time, scheduling delay, or processes appearing stuck without IO
evidence.

Do not run both load commands by default. Run the second command only when the
first output says the required entity is still missing or the user's symptom
clearly spans both load composition and scheduling delay.

## Network

Use `sysom-osops net packetdrop` for packet loss, retransmits, drops, connection
reset, or timeout symptoms. Use `sysom-osops net netjitter` for latency
fluctuation, jitter, or intermittent network degradation.

Stop when the envelope names the fault point, affected interface/path or flow
scope, evidence strength, and safe remediation or next action.

## Java Memory

Use `sysom-osops memory javamem` when the user explicitly reports Java heap, GC,
JVM memory, or Java process memory symptoms. If the symptom is only "memory high"
and Java is not explicit, start with `sysom-osops memory classify` so SysOM can
decide whether Java is the dominant route.

## Cross-Domain Hints

If a SysOM envelope suggests a different domain than the user's first wording,
trust the visible evidence. Explain the pivot and run only the focused next
command that fills the missing root-cause entity.
