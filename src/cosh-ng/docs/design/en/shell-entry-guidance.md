# Shell entry and guidance contracts

[中文版](../zh/shell-entry-guidance.md)

Ordinary Cosh displays the original six-line outline logo outside its welcome card. The card starts
with command and natural-language capabilities, followed by Shell, approval mode
and cwd. Short metadata fits on one row; long paths retain their own full-width
rows. At 80 columns the normal card uses four rows, or eleven including the logo
and separator.

`--ask` reuses the card without logo, cwd, health or upgrade notices. Both entries
show `/help` once and preserve relevant AI and approval state. Text uses the
terminal foreground, borders use neutral gray, and titles and help use cyan.
The logo preserves the original blue, cyan and light-blue row gradient.

The first recommendation disclosure has a separate notice with activity use,
privacy details and disable controls. It is marked seen only after a successful
flush. Optional startup checks show a useful Cargo.toml project hint and remain
quiet without findings; they do not run project commands. Health and upgrades
retain their own regions.

`runtime/startup/welcome.rs` selects content; `ui/welcome.rs` owns rendering. Cards
are capped at 80 columns and sized after wrapping. Plain output has no logo,
borders or enhancement escape sequences.

Initial questions preserve prompt snapshots and input ownership. Prompt erasure
requires a proven line origin and linear layout. Unknown origins and ambiguous
wraps fall back to a newline. `shell_host/bootstrap.rs` emits a separator before
an interactive `--ask` child starts and records the same bytes in the display
stream; the runner delegates startup without guessing or erasing prior output.

Prompt-origin detection permits only OSC 0/2 title sequences after a known CRLF,
with a 4096-byte lookbehind. Visible output or other controls keep the origin unknown.

Below 42 columns ordinary startup omits the logo and keeps the card title.
Bash failure invitations use two open lines: a cyan left accent and brand, normal-foreground action label and copy hint, followed by the intact
ask command. Terminal wrapping preserves the logical command. The guide stays in
the login banner and is not repeated after failures. Plain fallback and suppression are unchanged.

Failure diagnostics use existing context and permitted read-only checks before requesting missing information; manually copying terminal output is not the default next step. Native Cosh retrieves available bound output through evidence access; uncaptured Bash output remains unavailable. Start missing-evidence investigation with one focused check, follow up only when justified by new evidence, and stop when sufficient. Recommend and analysis-only restrictions remain authoritative.

Failure invitations use the short question “Analyze this failure” with an explicit `--context` event ID.
`config/guide/context.rs` reuses private files, nonblocking locks and atomic writes: at most 16 records,
usable for one hour. Only eligible, unsuppressed events are saved. Failed writes or the existing worker
deadline suppress the reference. Later writes evict physical records; there is no background cleanup
or promise of erasure exactly at expiry. Sensitive observations are skipped rather than redacted and
misrepresented as complete commands.
`--context` requires interactive `--ask`; ID, schema, fields, allowlist and time are revalidated before
startup, without a latest-event fallback. The initial request receives a separate untrusted JSON
`external_bash_failure` hint, distinguishing original and current cwd and marking output / expanded
argv unavailable. It does not create a Cosh ledger entry; acceptance or cancellation clears the pending
context. Bash prefilters tool/status; Rust validates bounded literal syntax and sensitive arguments,
without invoking a Shell to parse them.

ERR observes a failure fragment, not the user's complete input or proven top-level call;
a returned wrapper function can expose its last external command. Keep the recorder
limited to a pure ownership check and assignments, preserving user functions.
If the owned prompt slot is replaced or removed, stop recording payloads; an
observer that cannot safely uninstall may remain inert. Other known conflicts retire the observer;
do not add DEBUG tracing, history inference or input interception to improve attribution.
