# Manage Login Shell Registration

[中文版](../../../../zh/user-entrypoint/cosh-ng/cli/login-shell.md)

Use `cosh-cli login-shell` to inspect or explicitly configure Linux login-shell
integration. Running `cosh` manually needs no registration or account change.
Installing cosh, registering an available shell, choosing an account's shell, and
opting into Agent session integration are separate decisions. These commands do
not enable AW, start Herdr, change Agent hooks, or change runtime login identity.

## Inspect the installed entry

```bash
cosh-cli login-shell status --user alice
cosh-cli login-shell --shell /opt/anolisa/bin/cosh status --user alice
```

By default, the command selects `cosh` beside the running `cosh-cli` executable.
Use `--shell` when managing a different installation or when running a development
binary. It must be an absolute UTF-8 path without whitespace, `#`, colon, or dot
components. Registration preserves that public entry path: a symlink's resolved
executable is reported separately, so a provider replacement can retain the
account's stable shell path. The selected executable is never run for inspection.

The JSON `data` reports `shell`, `resolved_executable`, `executable`, `registered`,
`user`, `account_shell`, `using_accounts`, `changed`, `previous_shell`,
`retained_provider`, and `account_access_checked`. `account_shell` preserves the
account record's exact value, including an empty string. Without `--user`,
`user` and `account_shell` are null.
`registered` matches the public spelling in `/etc/shells` exactly.
`using_accounts` compares path components from `getent passwd` enumeration,
ignoring repeated separators without resolving symlinks;
non-enumerable directory-service accounts and aliases to a different path are not
covered. Check those separately before removing a shell.

`status` and mutation `--dry-run` need no root privileges and create no manager
lock or state files. They still require permission to read the selected paths.
Linux needs `/usr/bin/getent`; account mutations additionally use
`/usr/sbin/usermod` from the distribution's account-management package. Other
platforms return an unsupported-platform error.

## Register and select separately

```bash
cosh-cli login-shell register --dry-run
sudo cosh-cli login-shell register
cosh-cli login-shell set --user alice --expect-shell /bin/bash --dry-run
sudo cosh-cli login-shell set --user alice --expect-shell /bin/bash
```

`register` adds an existing executable to `/etc/shells`. It never selects that
shell for an account. `set` requires an explicit local account, an already
registered executable target, and `--expect-shell` matching the account's current
shell. Before a privileged `set`/`restore` (including a privileged preview),
a disposable cosh-cli child adopts that account’s UID, primary GID and NSS
supplementary groups and asks the kernel to check execute access. Inaccessible
parent directories, ACL restrictions and non-executable targets refuse the change.
The candidate shell is never executed, and the manager keeps its original identity.
`executable` only reports a regular file with at least one execute bit;
`account_access_checked` reports successful target-account permission preflight.
Unprivileged previews leave `account_access_checked` false: run a privileged
preview to complete that check before selection. The check does not guarantee
future permissions, executable format, interpreters, libraries or login startup.
No account is inferred from `sudo`, `HOME`, or the current login session. For an
account with an empty shell field, pass `--expect-shell ''` explicitly.

Actual mutations require root. Repeating registration or selecting the already
selected target is a no-op. `changed` describes the actual change, or the proposed
change in a dry run; dry-run state fields still describe the current state.
Account changes affect subsequent logins and do not restart the current shell.

## Restore an explicit target and unregister

```bash
cosh-cli login-shell restore --user alice --expect-shell /usr/bin/cosh --to /bin/bash --dry-run
sudo cosh-cli login-shell restore --user alice --expect-shell /usr/bin/cosh --to /bin/bash
sudo cosh-cli login-shell unregister
sudo cosh-cli login-shell unregister --if-missing
```

Use the account's previously reported shell or another deliberately chosen target;
`restore` does not guess or maintain a hidden previous-shell history. `--to` must
be an executable already registered in `/etc/shells`; `--expect-shell` must match
the current account state. If the target is already selected, repetition succeeds
without writing. If an administrator selected another shell, the expectation
mismatch refuses the change. An originally empty field must be restored by an
administrator's explicit registered-shell choice; `--to ''` is not accepted.

`unregister` refuses while an enumerated account uses the entry, even if the
executable was removed. `--if-missing` additionally retains registration whenever
an executable provider still occupies the entry, including a replacement package.
It is useful for explicit provisioning cleanup, but does not replace package
uninstall protections. Neither operation deletes the executable or account.

## Installation and provisioning boundaries

Use the same explicit command after an existing supported Linux installation:

- **RPM:** the package retains its registration and uninstall scriptlets; ordinary
  installation does not select an account shell. Use the management command for
  inspection or an explicit account change. Direct RPM removal retains the
  existing scriptlets and in-use-account checks. Safer replacement-provider
  cleanup is tracked separately in [PR #3367](https://github.com/agentic-os-org/ANOLISA/pull/3367),
  which is not included in this management command.
- **Raw:** the payload includes `cosh-cli`. User-scope installation does not write
  system shell configuration. If using an already supported Linux raw layout,
  invoke its installed CLI and explicitly provide the public entry when needed.
  An administrator may register/select it after reviewing path accessibility.
  This command does not expand the raw package's supported distributions.
- **Images:** after installing the payload in the image, run the command inside
  the image environment with explicit account and entry paths. There is no
  `--root` mode; passing an image-prefixed path from the host would manage the
  host's configuration instead.

This command does not replace or migrate RPM scriptlets. In particular, uninstall
cleanup must remain runnable after package binaries are removed; do not invoke a
deleted `cosh-cli` from a post-uninstall phase. Shared lifecycle extraction and
additional raw/provider removal integration remain separate work.

## Update and concurrency guarantees

Shell-table updates preserve unrelated entries, comments, symlink relationships,
mode, ownership, and extended attributes. Preparation occurs in the target file's
directory, followed by a flushed atomic rename and directory sync. Errors before
rename leave the original intact; a directory-sync error after rename is reported
even though the new contents are already visible. Hard-linked, oversized, or
changed-during-preparation tables are refused. Interrupted processes cannot leave
a partially written table; a force-killed process may leave a `.cosh-shells-*`
temporary file in that target directory for an administrator to inspect/remove.

A private `/etc/.cosh-login-shell.lock` serializes these commands and is retained
as a reusable lock inode. The command never locks the account database itself;
`usermod` owns that lifecycle. The expected-shell check is repeated immediately
before `usermod`, followed by a result check. It is **not an atomic compare-and-swap
against other administrator tools**. Coordinate simultaneous account or shell-table
administration; non-cooperating writes in the final check-to-commit window cannot
be excluded. Failed account-tool calls remain visible and are not silently retried.

## Machine-readable failures

`error.code` distinguishes `UnsupportedPlatform`, `LoginShellConflict`,
`LoginShellAccessDenied`, `LoginShellBackendError`, and `LoginShellIoError`.
Invalid arguments use `InvalidInput`; filesystem permission, missing-path and
timeout failures use `PermissionDenied`, `NotFound`, and `Timeout` respectively.
Account-tool failures use `LoginShellBackendError` with the original diagnostic.
Conflicts include in-use shells and administrator changes; inspect them before
retrying. No failure is automatically marked recoverable.
