# Telemetry Metadata Cache

The uploader owns a long-lived MetadataClient and calls clear_unreachable at
its existing round boundary. Cloned clients share both the curl failure latch
and the cloud-init full-JSON lookup state.

Successful `cloud-init query --all` JSON is cached for the client lifetime.
A failed spawn, command exit or JSON parse counts as one attempted lookup for
that round, rather than a permanent cached None. The next clear_unreachable
re-arms the attempt flag while keeping successful JSON. A mutex serializes
initialization across clones, so concurrent fields do not run duplicate full
queries. The immutable success cache never exposes a partially parsed value.

This change applies to the full-JSON fallback only. Specific cloud-init path
queries and direct metadata lookup retain their existing ordering and behavior.
Valid JSON lacking a requested field remains a successful cached document;
periodic refresh of successful metadata is outside this contract. No command
timeout or cloud endpoint behavior is changed.

The public API and uploader loop cadence are unchanged. Callers should reset
failures at a new round boundary, not for every field. Reverting the cache
change restores the previous permanent-failure behavior; no persisted schema
or configuration migration is needed.
