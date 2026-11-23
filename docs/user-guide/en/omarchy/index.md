# ANOLISA Omarchy user guide

[简体中文](../../zh/omarchy/index.md)

## Install a candidate before official publication

Use Omarchy Linux x86_64 with Python 3.11+. Obtain the verified archive and its
adjacent `.sha256` file from the same Preview run or review handoff. In their directory:

```bash
sha256sum -c anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.sha256
tar -xzf anolisa-omarchy-0.1.3-linux-x86_64.tar.gz
cd anolisa-omarchy-0.1.3
bash ./install
~/.local/bin/anolisa-omarchy status
```

Run as the desktop user in a graphical terminal, not as root. Keep the installer
archive and checksum for reuse. The checkout installer also accepts
`bash ./install --archive /absolute/path/anolisa-omarchy-0.1.3-linux-x86_64.tar.gz`;
the sidecar must be beside the archive. The same-version official URL will work
only after the maintainers publish the matching Release.

The installer checks hashes and runtime libraries, installs the root-owned safety
service with sudo, then registers the bar widget. It prints completion and setup
instructions. `--non-interactive` fails if sudo needs input. `--no-enable` deploys
user files without system dependencies, service setup or desktop activation.

## Configure and work

Click **ANOLISA → Start setup**, or run `anolisa-omarchy setup`. Each component can
be deferred. Return through **Settings → Set up cosh / Set up Qoder**.

- **cosh:** enter `1`, read the instructions, then press Enter to open cosh for
  configuration. Enter `/auth` and follow cosh's model-service prompts. These
  prompts belong to cosh; the plugin does not replace its model selection.
  Enter `exit` to return to the wizard.
- **Qoder:** continue setup, then choose recommended version 1.1.84, Latest, or
  setup later if no compatible CLI is available. **Configure in Qoder CLI** opens
  Qoder's browser/PAT flow. **Enter your Qoder PAT here** saves hidden input in the
  plugin's private configuration. Both can be reused on the same user account.
  They differ in who stores and supplies the credential. Neither proves a model request succeeded.
- **Language:** English is the default. **Settings → Language** offers English and
  Simplified Chinese. New cosh sessions receive `COSH_SHELL_LANG`. Qoder keeps its own UI.

Open the working terminal. Enter `/agent` on its own, press Enter, then type a
question at the Agent prompt. Esc returns to the shell; `exit` closes it.
Direct natural-language routing depends on cosh's active shell hooks. The plugin
sets `MISE_NOT_FOUND_AUTO_INSTALL=0` before shell startup to avoid mise replacing
cosh's missing-command handler. It does not edit `~/.bashrc`. Use `/agent` for an
explicit entry when diagnosing routing.

Open Qoder for file tasks. The plugin starts Qoder through `aw run`; AW connects
its native tool events to the sec-core and Tokenless Providers. cosh's own Agent
does not automatically delegate to that Qoder process.

```bash
anolisa-omarchy help
anolisa-omarchy status
anolisa-omarchy doctor
anolisa-omarchy setup --component cosh
anolisa-omarchy setup --component qoder
anolisa-omarchy qoder-install --channel recommended
anolisa-omarchy qoder-install --channel latest
```

Open **Help** in the panel footer for these commands and short component descriptions.
`status --json` and `doctor --json` are optional machine-readable reports.

## Local views and limits

sec-core shows up to 200 recent service scans and risk findings. A risk finding
does not by itself prove an operation was blocked; missing block evidence is
shown as `—`. This is Bash tool-hook protection, not an OS sandbox.

Tokenless shows up to 200 recent managed records for the current user. It estimates
reductions in supported tool output. Short or unsupported output may produce no
reduction. Missing data is `—`; a measured zero is `0`. These are not billing savings.

The dropdown is a launcher and local view. Work remains in ordinary Omarchy
terminal windows. Closing a terminal ends its foreground session; background task
recovery and a complete multi-task dashboard are not provided.

## Upgrade, paths and removal

Run the new archive's `bash ./install` to upgrade. Managed settings, credentials
and statistics are preserved. Edited managed files stop activation for review;
the installer prints the proposed configuration path instead of overwriting it.

- Programs: `~/.local/share/anolisa-omarchy/releases/`, with a `current` symlink.
- Command: `~/.local/bin/anolisa-omarchy`.
- Configuration: `~/.config/anolisa-omarchy/`; state: `~/.local/state/anolisa-omarchy/`.
- Bar widget: `~/.config/omarchy/plugins/community.anolisa/`.
- Root-owned daemon: `/opt/anolisa-omarchy/sec-core/`.
- Service: `/etc/systemd/system/anolisa-omarchy-security.service`.

XDG_CONFIG_HOME, XDG_DATA_HOME and XDG_STATE_HOME override ANOLISA's matching user paths.
The bar widget stays under `$HOME/.config/omarchy/plugins`, as required by Omarchy.
Qoder's native account files remain under its own user configuration. Preserve
these directories when collecting logs; do not include credentials in evidence.

```bash
omarchy plugin remove community.anolisa
sudo systemctl disable --now anolisa-omarchy-security.service
```

This removes the widget and stops the safety service. It preserves programs and
user data. There is no automatic full purge in this version. Do not mix this
user-prefix installer with a native pacman package; remove the old installation
mode before changing modes.

## Recover an interrupted installation

If an interrupted system-service transaction prevents installation, stop other
installers first. From the same verified extracted archive, run:

```bash
bash ./install --recover
```

This restores the recorded previous service state, then retries installation.
The checkout entry also forwards `--recover`, including with `--archive`.
Do not delete the root-owned transaction journal manually. Ordinary caught
installation errors already attempt rollback and do not always need recovery.

The installer embeds build-time hashes for its privileged helper and daemon.
It runs a verified root-owned helper copy under isolated Python. Obtain the
installer and checksum from a trusted publication; these checks do not authenticate
an installer that was already replaced before you ran it.

## Acceptance

1. On a clean Omarchy instance, record `/etc/os-release`, `uname -m`, Python version,
   candidate SHA-256 and a complete installation transcript. Check `status` and `doctor`.
2. Open the native bar widget. Check anchoring, outside-click close, Home → Help,
   Settings → Help, English/Chinese switching, and ordinary terminal focus.
3. Configure cosh, obtain an actual model answer, exit, reopen and repeat without
   entering the key again. Record success without copying the key or account file.
4. Install recommended Qoder, configure its account, reopen, and run a disposable
   Bash file task. Check the result file, AW/native tool events, service scan count
   and a supported long-output Tokenless record. Keep failed attempts as well as retries.
5. Save settings, shut down normally, and start the same VM. Check service startup,
   credentials and view data. Reinstall the same candidate and repeat. Cross-version
   migration requires a separate test; same-version reinstall does not prove it.
6. Remove the widget and stop the service. Confirm they are absent/inactive, then
   reinstall and check that the intended account/settings data was preserved.

If HTTPS fails with a certificate-chain error, correct the VM's trusted corporate
CA or use a verified offline archive. Do not disable TLS or package signature checks.
Use `journalctl -u anolisa-omarchy-security.service` for service errors. Keep reports
and redacted transcripts together with the exact candidate hashes.
