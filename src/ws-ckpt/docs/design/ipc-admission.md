# IPC admission

[中文版](ipc-admission_zh.md)

The listener admits a single administrative trust domain before decoding any
legacy or guarded request. Root is implicit; additional UIDs come from the
daemon's startup environment. Kernel peer credentials are mandatory. This
stage does not assign workspace owners or grant operation-specific rights.

The allowlist is parsed once before binding the socket. Rejections close the connection rather than
adding an error variant older bincode clients cannot decode. Clients must not
assume a response timeout means a mutation was not executed.

Frame parsing and execution retain their existing semantics. Connection quotas,
byte budgets and transport deadlines are deferred to a separate stage so this
change can coexist with the structured malformed-frame response work.

The root-only Linux socket test connects from two real non-root UIDs, accepts
the allowed UID and denies the other UID. It uses the real listener with a temporary backend and does not
require mounted Btrfs or exercise checkpoint persistence.
