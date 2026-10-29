# Hermes native Hook adapter

[中文版](../../zh/adapters/hermes-adapter.md)

The Hermes adapter connects an existing local profile to the AW service through
Hermes' native plugin and shell Hook APIs. It targets the official
`NousResearch/hermes-agent` revision
`952c941e741e922a9be8fc403c8944c6e96318bb` and the Python `chat` entrypoint on Linux.
Gateway, ACP, Desktop and the separate TUI entrypoint remain unsupported.
AW inserts native `--cli` so ambient TUI preferences cannot change that entrypoint.
Only complete supported chat options are accepted; profile changes, worktree and
resume switches, plugin-disabling flags and native option abbreviations are rejected.
Profiles that redirect the CLI through `.container-mode` are also rejected.
Explicit executable paths and PATH candidates share regular-file and effective
execute-access checks. The selected console script is opened nonblocking, checked
again through its descriptor and read with a 64-KiB bound, so replacement with a
FIFO cannot block shebang discovery. Before executing any probe, AW requires the
script, interpreter and their path components to be owned by the caller or root,
with no group/other writes except sticky directories. Symlink targets are checked
too. AW pins the resolved script path while retaining the venv interpreter path;
it never changes native file permissions.
Source discovery uses the console script's venv interpreter with `-I -S` and
standard-library distribution metadata. The venv and editable-checkout paths
require UTF-8; non-UTF-8 installation paths are unsupported and fail discovery
before native imports or publication. It reads the local editable `hermes-agent`
installation's `direct_url.json` without importing Hermes, processing `.pth` files,
or loading project/environment import paths. Missing or ambiguous installations
are rejected. The venv's `pyvenv.cfg` must also be protected and must not enable
system site packages. Before any `-P` probe, AW walks the discovered venv site
directories, including `.pth`, finder modules, packages and caches, applying the
same ownership/permission checks to files and links. Site directories must not
be group- or other-writable, even with the sticky bit: new import names could
still be added. This scan is cancellable
and bounded to 30 seconds. Directory discovery includes both native site paths
and the explicit venv scheme, including Python versions where `-S` changes prefix
initialization. Every native Python command receives `-X pycache_prefix=...`
before interpreter startup, including site initialization. Preflight probes and
the install writer share a mode-0700 temporary cache whose path and ancestors
pass the native ownership/permission checks before use. This rejects a custom
non-sticky writable `TMPDIR` parent; the cache is removed on success, error or
cancellation. Checkout/profile bytecode caches are neither read nor modified.
The configured native environment remains intact for later commands.
The Git helper uses the same executable resolver and ownership/permission checks
as the console script. Resolution is checked from both the launch cwd and the
installation checkout, matching native Git calls; safe relative PATH entries
remain supported. Skipped PATH names are checked too: existing candidates must
be protected, and absent candidates require a protected nearest existing parent.
Sticky writable parents cannot admit new higher-priority executables.
AW pins its helper to the checkout-resolved absolute path and
sets `GIT_WORK_TREE` to the discovered source for inspection, so `core.worktree`
cannot redirect checks to a different clean tree. These overrides do not rewrite
the final native environment. The checkout and launch repository share validation of the `.git` locator,
the derived Git directory's optional `commondir` locator and local configuration
names before reading effective configuration. Git also resolves the automatic
`post-index-change` Hook path, including `core.hooksPath`, for the same protected
file/name checks. Native version/status probes can refresh the real index before
AW callbacks are ready; protected Hooks retain their native behavior.
An explicit `core.hooksPath=/dev/null` continues to disable Hooks.
Ancestor lookup honors `GIT_CEILING_DIRECTORIES` before and after dotenv loading;
the current directory and repositories below a ceiling still receive the same checks.
Relative configuration origins and includes resolve from the selected repository,
matching Git when launched from a subdirectory.
An absent launch `.git` name
requires a protected parent; existing launch gitfiles must themselves be protected. Explicit `GIT_DIR` and `GIT_COMMON_DIR` selectors
are unsupported in either the inherited or dotenv environment; unset them before
retrying. Inspection removes `GIT_CONFIG` only from its private command environment:
that variable selects a file for `git config`, while status reads normal scopes.
Filter and fsmonitor checks therefore describe the configuration used by status.
The eventual native environment stays intact. Each checkout
index/status probe passes `-c core.fsmonitor=false`, overriding repository and
included settings without editing them, so inspection cannot invoke that hook. Before
index/status checks or native imports, AW also reads Git's configuration origins
and applies the same ownership/permission checks to every loaded configuration
file, including includes. Git also expands explicit include paths; relative paths
resolve against their configuration origin. An absent target requires a protected
nearest existing ancestor, including conditional include references, so another
user cannot create a configuration after inspection. Native version and timeout probes run their own Git
commands; writable configuration must be rejected before reaching those imports.
Relative external scope selectors are unsupported because native Git runs from
both the launch cwd and source. Before each checkout inspection, require absolute
nonempty `GIT_CONFIG_SYSTEM`, `GIT_CONFIG_GLOBAL`, `XDG_CONFIG_HOME` and any set
`HOME`; use absolute paths or unset them before retrying. Empty system/global/XDG
values keep native disabling/default semantics.
Scope paths are checked even when absent: explicit system/global overrides,
Git-expanded HOME/XDG global paths, and `rev-parse --git-path` repository/enabled
worktree paths. Disabled scopes and `/dev/null` remain supported. For an unspecified
system path, Git must report its compiled location with `var GIT_CONFIG_SYSTEM`;
older Git requires an explicit `GIT_CONFIG_SYSTEM` path or disabled system scope.
AW does not guess a custom Git build's system directory or rewrite the environment.
The effective `core.fsmonitor` must be absent or boolean, as parsed by Git.
External command hooks are unsupported even with a protected configuration file;
disable the hook before launching. This avoids treating a shell command as one
trusted executable path. Configured external Git `filter.*.clean`, `.process` and
`.smudge` commands are also unsupported and rejected before index/status probes.
These rules apply before and after dotenv loading.
The checkout check runs first in the parent environment. A bounded Python `-P`
probe then calls the pinned `load_hermes_dotenv` with external secret fetching
disabled, and returns only Git-related variables, `LD_*` loader variables, and
`PATH`, `HOME` and `XDG_CONFIG_HOME`. Nonempty `LD_*` values are rejected before
native subprocesses and again after dotenv loading, before the second Git probe;
empty/unset values remain supported. The same executable, configuration-origin and checkout checks
run again with that effective environment before CLI version/timeout imports.
Dotenv precedence and interpolation stay native; the final chat/provider
environment is not rewritten by this probe. Native startup still performs its
normal dotenv and external-secret loading.
Before that site-enabled probe, the project dotenv check rejects a group/other-writable
checkout root even when sticky, with or without a `.env` file. This also protects
new import names at the pinned checkout root activated by an editable `.pth`.

Before executing `--version`, both installation and launch check the discovered
checkout's paths and tracked files, the pinned Git commit, index flags and clean
tracked status. A bounded `git ls-files -v -z` probe rejects `assume-unchanged` and
`skip-worktree`; clear those flags before retrying. Every checkout probe uses
`--no-replace-objects` so worktree/index comparisons refer to the pinned object,
even if replacement refs exist. AW checkout probes use `--no-optional-locks`
to prevent optional index refreshes and leave replacement refs unchanged.
The status probe overrides `core.trustctime=true` and `core.checkStat=default`
and additionally compares against a temporary index populated from HEAD.
The fresh index has no cached stat data, so same-length edits with restored
mtimes are detected even on Git builds with second-resolution stat checks.
Writing the scratch index passes `-c core.hooksPath=/dev/null`, disabling
`post-index-change` and other checkout hooks only for that inspection command,
including configured `core.hooksPath` overrides. Original hooks and configuration
remain unchanged for native operations.
The temporary index directory is explicitly mode 0700 and removed on success,
error or cancellation.
These overrides apply to AW inspection only; native Git commands retain their
normal environment and behavior.
`git status --porcelain --untracked-files=no` must then be empty. These checks are
bounded and cancellable and permit untracked environment/build files; they are
not a package integrity audit.

The version probe runs the checked script with its verified interpreter and `-P`,
excluding implicit imports from the script directory even when it is sticky and
writable. Caller-managed Python configuration is trusted, including explicit
`PYTHONPATH` entries and protected `.pth`/editable-finder code. AW preserves that
configuration and does not recursively audit the external import roots or
dependencies it selects. The installation owner must trust those configured
paths. The checks above verify the pinned framework checkout and AW-owned launch
artifacts; they do not sandbox the installed Python environment.
The subsequent version probe allows up to 60 seconds because this Hermes revision
performs its own update check. AW does not change that setting or retry failures.
Its reported installation directory must match the already checked metadata source;
a changing `origin/main` banner is not used as the version.

The metadata discovery probe uses the existing bounded data runner with a 4 MiB
JSON output limit, a 5-second deadline and cancellation; version strings retain
their 4 KiB limit. Source and site paths are still checked before native imports.

The dotenv probe carries selected environment names and values as byte arrays,
using Python filesystem encoding and Rust OsString to retain Linux environment
bytes, including non-UTF-8 PATH components. The JSON uses the existing 4 MiB data-output budget with a
5-second deadline, cancellation and bounded stderr; version output remains capped
at 4 KiB. Both inherited and profile-defined PATH values above 4 KiB are covered
through the shared preflight and actual install/chat entrypoints. The limit applies
to serialized JSON, including byte-array overhead; environments exceeding that
serialized budget remain outside this bounded probe contract.

## Installation and launch

The shared launch entrypoint requires a UTF-8 canonical working directory before
adapter preparation or service setup. Canonical profiles have the same requirement
before installation; JSON launch/status paths cannot fail after publication.

Hermes reads `config.yaml` from its profile directory. This revision has no
temporary configuration overlay and only loads enabled plugins. AW therefore
uses an explicit installation step:

```bash
aw install --config aw.yaml --agent hermes --native-profile /absolute/profile
aw run --config aw.yaml --agent hermes --native-profile /absolute/profile -- chat
```

The configured Agent `argv` supplies the installed Hermes Python console script
with an absolute Python shebang. Shell wrappers and `python -m` entrypoints are
unsupported; use the console script generated by the pinned package installation. Put `chat`
and its arguments either in `argv` or after the launcher's `--`, once. The
[sample configuration](../../../../crates/aw-service/examples/aw.hermes.yaml) puts
`chat` in `argv`.
Installation checks any configured native arguments with the same `check_args`
contract as launch, before probes or profile writes. An executable-only `argv`
is valid because launch can supply `chat` after `--`.
Adapter selection and installation use the same validated AW configuration
snapshot, even if an editor replaces the configuration file during dispatch.

Installation writes the bundled plugin to `plugins/aw-native-hooks`, adds that
name to `plugins.enabled`, and removes its matching `plugins.disabled` entry.
For lists containing `${...}`, the pinned native expander resolves selections in
the live profile environment. Entries resolving to AW are normalized to its
literal enabled name or removed from disabled; other templates stay unchanged.
The native probe returns only the indices resolving to AW, so interpolation
growth does not consume the JSON output budget or expose expanded unrelated values.
Both the expander and YAML writer receive JSON lists through bounded stdin,
using the existing 4 MiB data budget rather than an OS argument.
Raw installation files and serialized JSON input have separate 4 MiB limits;
escaping or YAML expansion does not exempt a payload from its transport limit.
Oversized serialized selections fail before native writing or profile publication.
Unknown configuration values and existing plugin selections remain intact. The
configuration update saves the exact original bytes in a private
`config.yaml.aw-backup-<id>` file. Only the required plugin selection fields are
updated in an owned private staging directory. Installation resolves YAML merge
sources before reading effective plugin lists,
including nested merges, sequence precedence and explicit overrides. The same
reader validates the candidate and installed configuration. The pinned native
round-trip YAML editor copies the target plugin mapping before replacing a list,
so shared anchors retain their original values. Its YAML 1.1 resolver quotes
names such as `on`, `no` and `null`, keeping them strings in the runtime reader.
The writer preserves unknown values, comments and quotes, and flushes and syncs
each new file before replacing the staged candidate. The writer initializes the selected native profile
and its dotenv, and retains its managed-installation and per-key edit checks.
Only its explicit YAML path points to staging; `HERMES_HOME` and the profile
selector continue to identify the live profile. The writer and callback-timeout
probe use Python `-P` so the project directory cannot implicitly shadow native
imports; explicit interpreter environment settings remain intact. AW
creates the backup only after candidate validation and the final live comparison;
staging failures leave no backup copies. Publication errors report its exact path.
AW compares the live file with the original bytes before atomically exchanging it
with the candidate. Failed writer commands and edits detected before publication
leave the live file untouched. The exchange retains the displaced inode in
`config.yaml.aw-backup-<id>.displaced`, reported as `displaced` alongside `backup`.
An edit in the final comparison/exchange window produces a visible conflict:
the candidate is live and the concurrent version is retained for recovery. AW
does not automatically roll back over subsequent edits. Keep the displaced file
until other writers have closed their descriptors; late writes to the old inode
remain there. Native writers do not honor AW's lock, so this preserves their data
without promising a conditional content replacement. Unsupported exchange fails
without falling back to an unconditional rename.
Staging contains only the configuration candidate, without profile credentials
or history. After publication, installation explicitly removes that staging
directory and syncs the profile before reporting success. Cleanup or sync errors
report the published state and both recovery paths, without rollback. The Drop
guard retains best-effort cleanup on error paths. Each writer command has a
30-second deadline. Installation shares the launch signal handler and cancellation
flag: an interrupt reaps native probes/writers and unwinds staging cleanup before
returning the signal exit status. Installation
checks the caller-owned cancellation flag before entering installation, creating
plugin resources and each atomic publication, including paths with no native
writer. Cancellation removes unpublished staging and displaced links; an initial
backup already created is retained and reported. Published plugins and exchanged
configuration are not automatically rolled back; completed publications finish
their directory sync and recovery checks. The lock file and an empty `plugins`
parent may remain for a later retry. Repeating installation
with unchanged files is a no-op. A different plugin under the same name,
symlinked installation files, malformed plugin lists, or an occupied installation
lock cause a visible failure. A scoped guard explicitly unlocks before closing the
installation descriptor, so concurrent fork/exec cannot prolong a completed
installation lock. Profile configuration and plugin files must be
caller-owned regular files without group/other write permission; modes 0600 and
0644 are accepted. The same no-follow reader validates optional profile and checkout `.env` files
before native dotenv or CLI imports. Their parent directories must prohibit
group/other writes even when sticky, to protect an absent dotenv name. Dangling
symlinks and non-regular files are rejected rather than treated as absent.
Canonical profile paths must be UTF-8 before any probe or installation write.
AW does not change an unsafe file's permissions. The canonical profile's ancestors
must be owned by the caller or root, without group/other write permission unless
the sticky bit protects their children. This rejects paths another user can
replace before native plugin import, while allowing private profiles under `/tmp`.
Plugin directory publication uses `RENAME_NOREPLACE`, so even an empty directory
created by a concurrent native installer is preserved and reported as a collision.
Configuration conflicts detected before backup creation report the live conflict
without naming a nonexistent recovery file.
Creating the `plugins` directory also syncs the profile directory, including when
the existing configuration already enables AW and no configuration write is needed.
Launch and installation share bundle validation and report the same explicit
backup-and-reinstall procedure for mismatched or unreadable/missing bundle files;
the diagnostic identifies the file and preserves the read error. They never
overwrite such a bundle automatically.

After publication, AW prints the installed-state report with both recovery paths
before removing the private Python cache. A cleanup failure still returns an
error, but cannot hide an installed configuration or its recovery files.

Installation does not invoke `hermes plugins enable`: that command can hot-load
plugins into an existing Gateway or Desktop process. AW only updates local files.
It never copies or rewrites the profile's `.env`, `auth.json`, databases, memory or
sessions. Normal Hermes operations can still update their own state.

An empty `providers` and `events` configuration launches with zero AW callbacks.
Existing native Hooks still run; AW records the session lifecycle without
claiming per-tool policy checks or audit records. Explicit installation remains
required.

Launch requires the matching installed plugin to be enabled. It keeps the
selected `HERMES_HOME`, pins the equivalent native `--profile` selector so a
sticky `active_profile` cannot redirect the process, and retains the launch
working directory. The installed plugin is inert during ordinary Hermes launches
without the AW launcher's process-local session binding.

## Hook semantics

Each admitted AW step becomes a separate native shell Hook callback registered
with `PluginContext.register_hook`. Hermes serializes callbacks in registration
order and retains control over parallel or sequential tool batches. Existing
configured Hooks still run; AW does not import or reorder their configuration.
Native Hook configuration has no per-Hook environment object in this pinned
revision.

Raw commands receive the actual callback process environment, including variables
loaded from the profile's `.env` and Hermes' native HOME/TMPDIR settings. Structured
Providers retain the environment captured at launch binding. AW does not put
environment contents into normalized events or audit records.

| Native event | AW event | Available behavior |
| --- | --- | --- |
| `pre_tool_call` | `tool.before` | Structured observe/block; native commands retain native stdout and exit semantics |
| `post_tool_call` | `tool.after` | Observation, including blocked, failed, cancelled and timed-out tool outcomes |

The native payload supplies `session_id`, `extra.api_request_id` and
`extra.tool_call_id`. The normalized call ID hashes the request and tool-call ID
together, so a model can reuse a call ID in a later request. AW rejects
missing identities, preserves arbitrary tool names and object arguments, and
keeps `extra.result` in its native type, including serialized JSON strings. No
generated fallback replaces a missing native identity.

Hermes parses a native `action: block` response or exit code 2 as a veto. A veto
does not prevent later registered before callbacks from running. Native
`action: approve` requests human approval; it never means automatic allow.
Structured AW `ask` remains unsupported. When AW before callbacks are configured,
the launcher delegates to the native directive reducer, then blocks any effective
argument rewrite and discards its modified arguments. Native callbacks inspect
the original arguments and merge rewrites afterward; executing a rewrite would
bypass the AW check. All callbacks still run, and existing native veto reasons
are retained. This guard survives plugin rediscovery and is restored on native
main exit. The same launch scope wraps native tool-execution middleware: freeze
the JSON arguments before the middleware chain and return a native tool error
instead of dispatching changed arguments. This includes `execute_code`'s nested
`CellAuthority`/RPC calls, where pre-tool checks precede execution middleware.
Replacing a dictionary with identical JSON values remains supported; nested
in-place and value-type changes are refused. Only native JSON value types and
string dictionary keys can reach dispatch with AW before callbacks; tuples and
integer keys cannot collide with the inspected JSON. Integers must fit i64/u64,
floats must be finite, and string values/keys must contain Unicode scalar values;
unsupported inputs are rejected, not converted. Execution rewrites are unsupported
for both top-level and nested calls when AW before callbacks are active, using
one shared native middleware boundary without replaying policy callbacks.
Native after callbacks observe the resulting tool error. Both guards survive
plugin reload and are restored on main exit. Empty and after-only AW routes
retain native rewrite behavior.
Post callbacks cannot undo the tool or replace its result.

Both the AW event deadline and Hermes' callback timeout apply. Launch refuses
event budgets whose native command timeout and cleanup allowance exceed
`plugins.hook_callback_timeout`. This admission includes synthesized observation-only
callbacks as well as configured action callbacks. A bounded probe uses the console
script's interpreter and the pinned native CLI profile/dotenv loader, then queries the
native timeout resolver. YAML 1.1 scalars, environment expansion, the default
and the 600-second clamp therefore match Hermes. Launch probes share the launcher's
cancellation flag; SIGINT/SIGTERM promptly reap their process groups and retain
the signal exit status. Installation and launch preparation share error reporting
before signal restoration, including failed process-group cleanup diagnostics.
A private readiness receipt records successful
registration before the launcher reports native Hook readiness. A transient
startup guard uses the console script's Python interpreter in the same process;
its interpreter starts with bytecode imports directed to a cache under the
private owned launch directory, then the guard restores native argv before
importing the native CLI. Previously
cached profile bytecode is not read; existing caches remain untouched and the
private cache is removed with the launch directory. Profile selection
and dotenv loading remain native. The guard captures the launcher-provided
`AW_HERMES_LAUNCH` before importing the native CLI and stores that path in a
launcher-owned process-local module before plugin discovery. Registration and the
receipt check use that captured path. The module survives native plugin teardown
and is removed when native main returns; later dotenv reloads cannot select
another session's binding or callback set. An environment variable alone does
not activate the plugin in an ordinary Hermes process.
At the shared chat startup seam, it rejects
effective safe mode, preserves native startup, joins native plugin discovery,
and requires the matching receipt before chat dispatch. It also verifies the
plugin at the installed profile path is loaded/enabled without an error and
its active native ownership ledger matches the expected Hook event counts. A
receipt from an abandoned plugin loader cannot admit chat; even an empty-policy
launch requires the plugin to remain enabled. It preserves cwd and
PID; the external readiness timeout only bounds startup. This remains same-user
installation evidence, not a final or protected security boundary.

Native forced rediscovery unloads the old Hook registrations and calls the plugin
again. Each call registers the full callback set with the new native context.
Readiness is created atomically on first registration; later calls reuse the
existing receipt only when its complete contents match the current session
(including token, PID and Hook count). The shared bounded, no-follow reader
requires a caller-owned regular file with mode 0600. A mismatched or unsafe
receipt fails registration without overwriting it. Receipt reuse does not skip
callback registration or checks of the live native registry. The launcher reuses
that check at the native before reducer, tool-execution boundary and shared
terminal post emitter, joining
native discovery before reading the ledger. If rediscovery omits AW or its load
fails, a stale receipt cannot admit another tool dispatch: the session exits
before execution. After-only routes require their audit callbacks too; empty
routes keep native behavior. The execution wrapper checks again before its
downstream handler, without replaying policy callbacks. With AW callbacks, it
counts in-flight before-check, execution-middleware and post-dispatch calls under the native discovery lock and
refuses forced rediscovery before teardown while any are active. This preserves
callbacks within those boundaries even when another thread, a handler or
middleware requests rediscovery. If AW disappears between a veto and its post
event, the terminal emitter exits before the native no-listener gate can silently
skip audit. Registry and inline tools share this emitter. The lock only protects counter updates and
discovery; it is not held across callbacks or tool execution.
The native dispatcher retains error/result reporting; no callback replay or
replacement scheduler is added. Reload between tools and empty routes retain
native behavior. The discovery method is restored on main exit.


## Verification

Rust tests cover profile installation, byte-exact backups, idempotence, unknown
configuration values, collision and symlink rejection, event identities and
entrypoint restrictions. The optional native tests use the pinned official
Hermes Python environment with temporary profiles.

The native suite also exercises the STT availability probe used by `/voice status`
and an explicit forced rediscovery after successful registration. Both reloads
must retain blocking, after-event audit callbacks and existing configured Hooks
without duplicates, even after native dotenv loading replaces the launch variable
or the variable is removed. Empty-policy re-registration and invalid receipts are covered.
Rewrite regressions cover the modifier before and after AW registration, both
sides of reload, existing veto reasons, and native rewrites for empty/after-only routes.
The real native `CellAuthority.dispatch` path also covers execution-middleware
replacement, nested mutation, boolean-to-number changes and observation-only
middleware before/after plugin reload, including after-event errors and empty
or after-only routes. Failed forced-reload tests use real discovery to omit AW
or time out its loader after successful startup: before+after and after-only
routes cannot reach the final handler, while empty routes retain native behavior.
Only the final registry handler is replaced by a recorder.

```bash
python src/aw/crates/aw-cli/tests/fixtures/hermes/native_contract.py \
  --source /absolute/hermes-agent \
  --plugin /absolute/anolisa/src/aw/crates/aw-adapter/native/hermes
```

`live_cli.py` exercises official `chat --oneshot` with a deterministic local
OpenAI-compatible endpoint, a real AW daemon and the sample Provider. It runs in
the pinned Hermes Python environment with `--source` selecting that checkout. It checks
allow/block/empty-policy adoption, native dotenv safe-mode precedence, existing
relative-path Hooks, before/after commands,
retained profile state, native YAML scalar meanings, callback environment,
instance release and cleanup. It uses no real model key
and does not certify a hosted model or interactive human approval. A test plugin
triggers the native STT rediscovery at the next API request after the first
tool's post event has completed; a second
tool attempt must still be blocked. The real AW journal must contain matching
before/after events and successful policy calls for both attempts.
Installation also retains YAML 1.1 string names such as `on`, `off`, `yes` and `no`
and byte-exact repeated-install idempotence. Each template-containing plugin
list exceeds Linux's 128 KiB single-argument limit and passes through native
expansion and writing. A non-root filesystem fault denies private-cache cleanup
after installation; the command must fail while reporting installed state and
readable, byte-exact backup/displaced paths. The fixture restores permissions
and removes its cache. The rewrite chat case must block a
coexisting Hook's rewrite even when the original arguments are allowed, with no
tool side effect and matching before/after audit events.

## Rollback

Stop sessions and configuration writers before restoring a file reported by
`aw install`. Review `displaced` for concurrent or late writes and merge them;
`backup` holds the exact initial bytes. Restore the chosen file over the same profile's `config.yaml`, then
remove only `plugins/aw-native-hooks` and `.aw-install.lock` created by AW. Keep or
delete both recovery files according to the operator's retention policy. Ordinary
Hermes launches can also retain the installed plugin because it is inactive
without an AW launch binding.

Launch JSON rejects non-UTF-8 AW executable, binding and state paths with a
fallible error. Installed-state output uses a fallible writer; failures report
the published state, backup and displaced paths through the CLI error channel.

Native Git resolution from cwd and checkout must select the same canonical
executable; relative PATH entries selecting different builds are unsupported.

Checkout and launch cwd share one effective Git-configuration checker. Each
uses its own cwd for local origins and includeIf activation; only checkout
inspection pins the worktree. The launch check never runs status or updates
the project index. A distinct ordinary project repository remains supported
when its configuration passes the same existing filter/fsmonitor/path checks.
