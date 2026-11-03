# ANOLISA adapter source selection

ANOLISA retains ownership of its receipt-bound user extension copy. Cosh owns
source selection and activation state, including source identity validation and
transaction rollback. Native local path installations currently cannot update
in place, so the adapter continues its existing file delivery lifecycle.

The `extensions` registry domain exposes two adapter integration actions:

- `adapter-capabilities` returns `conditional_clear_user_source: true`.
  ANOLISA probes this before enable or disable changes extension files.
- `clear-user-source` requires `name` and `source_identity`. It removes a
  selection only when it names that exact user identity. An absent extension,
  absent selection, or later choice of another source is safe to retry.

Cleanup uses the existing locked catalog state transaction and validates the
candidate runtime before committing. Validation failure restores the previous
state. It preserves disabled extensions and follows normal runtime generation
publication. ANOLISA keeps its receipt when file removal or selection cleanup
fails.

For an identity-changing re-enable, ANOLISA releases the old user copy and
selection through its existing disable path before replacing the durable
receipt. Cleanup failure retains the old receipt; activation failure after
replacement retains the new receipt. Re-enable with the same effective identity
and destination keeps its existing in-place delivery path.

Enable verifies the discovered user identity, calls `select-source` and
`enable`, then reads `detail`. Status checks identity, desired state, native
health, and receipt files. Standalone registry calls cannot observe an existing
session, so loaded resources remain unknown and existing sessions need restart.
`COSH_HOME` aligns user extensions and state roots; the existing
`COSH_STATES_DIR` override retains precedence for state.
