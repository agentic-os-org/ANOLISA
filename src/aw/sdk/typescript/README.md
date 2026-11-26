# AW TypeScript SDK

`@anolisa/aw-sdk/profile-source` applies an
`aw-instrumentation-profile/v1alpha1` document to JavaScript SDK exports and
class methods on Linux with Node.js 22 or later.

## Bind a profile to AW

```js
import { readFile } from "node:fs/promises";
import { ProfileSource } from "@anolisa/aw-sdk/profile-source";

const document = await readFile("/opt/app/app-profile.json");
const source = await ProfileSource.probe(document, {
  resolveFrom: "/opt/app/server.mjs",
});
source.install();
await source.verify();
await source.activate({
  endpoint: "/run/user/1000/aw/aw.sock",
  target: "my-app",
});

try {
  const { run } = await import("./application.mjs");
  await run();
} finally {
  await source.teardown(5000);
}
```

Set `cooperative: true` on activation to wait for the existing
`aw connect --sdk-pid` control path. Pass `descriptorFile` and `configFile` to
bind through a trusted SDK deployment descriptor.

The source supports `session.start`, `input.submit`, `tool.before`,
`tool.after`, `turn.stop`, and `session.end`. Negotiation uses the effects
supported by all targets for an event and the existing SDK payload format.
The SDK rejects a `turn.stop.output` mapping during probe because the SDK
callback carries turn identity and reason. A routed `tool.before` checks its
input at native dispatch, after all Hook waits. Argument-backed prompts and
inputs use detached field paths, preserving the native text or JSON encoding
and null-prototype dictionaries. Rebuilding an array path preserves its array
prototype and own descriptors without running the subclass constructor. Map these fields to arguments when an
asynchronous call needs isolation from a mutable receiver.
A target can extract its session and turn identities from each call.
`resolveSessionId` and `sessionStorage` can supply a session ID when the field
has no value. Synchronous calls keep their return type and wait for policy on
an SDK worker thread. Iterator result replacement and withholding use the
profile's bounded buffer when the Scope routes `tool.after`. With that route
disabled, the wrapper delivers each native chunk as it arrives.

## Install before application imports

Set `NODE_OPTIONS="--import @anolisa/aw-sdk/profile-preload"` and set
`AW_PROFILE_BOOTSTRAP` to an absolute path. The bootstrap file must be owned by
the application UID, have mode `0600` or `0400`, and fit within 1 MiB. Use paths
without symbolic links. The JSON object contains `profile`, optional probe
settings `resolveFrom` and `availability`, and activation fields such as
`endpoint` and `target`. Omit the activation fields to install and verify only. The
preload consumes `AW_PROFILE_BOOTSTRAP` and removes its own import option so
child Node processes inherit the application's other startup options.

Module-level ESM exports must be intercepted before application import.
Imports within the target package use native ESM bindings, including cycles.
Writable CJS path segments need configurable data properties. Ordinary
assignments validate the replacement owner and install its method guards before
publishing it. A failed assignment restores the candidate’s original properties.
Use `guard.bind(...)` when assigning a bound method. The destination checks
the effective receiver and arguments, including pre-bound arguments. A borrowed
intrinsic bind produces an opaque callable, which the setter rejects because
its underlying target cannot be verified. A setter preserves a receiver's
existing read-only property. When explicitly invoked via the prototype with
`Reflect.set`, JavaScript reports accessor invocation as successful even if
the receiver cannot be written. Descriptor redefinition and deletion are outside
this binding contract. Earlier
references to a native function and calls through an SDK's internal bindings
can bypass its exported boundary. After teardown, calls through an existing
ESM facade forward to the current native function. The forwarding function has
its own identity. Static field lookup and enumeration follow the current class.
An active guard takes its callable or constructor shape from the native export
at installation. JavaScript fixes that shape for the life of the reference.
The facade for a live export or a callable with a locked own method reports
its descriptors as configurable and must
stay extensible so a replacement export can expose a different set of keys.
A separately cached required-mode guard still rejects calls. An async iterator enters its Scope on the first
`next()`. An admitted stream keeps the Scope open until it completes, exits,
or is cancelled. If required cleanup exceeds the
deadline, teardown reports `source_cleanup_unconfirmed` and cached wrappers
continue to reject calls.
