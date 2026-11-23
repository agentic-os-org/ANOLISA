# System integration assets

Standalone startup assets for the image or system-configuration package owner. These files are not installed or owned by the Cosh RPM.

- `profile.d/cosh.sh` is the source for `/etc/profile.d/cosh.sh`; the future owner RPM controls installation, configuration preservation, and removal.
- `bashrc-source.bash` is the protected include for the native initialization tail in `/etc/bashrc`; the future owner RPM controls that system-file change and preserves customer configuration.
- Cosh provides the compiled commands and its version-matched Bash support module. An image must explicitly integrate these assets to enable automatic login guidance.
- Tests stage the profile asset independently. Building or installing Cosh alone must not modify system startup files.

## Runtime compatibility

No initialization manifest, user-configuration digest, locale allowlist, or Cosh release-version match is required. The helper verifies only its own installed Bash module: root-owned trusted path, safe permissions, and bytes matching the module compiled into the executable. Its protocol-2 identity remains valid across harmless rc/profile edits and normal HOME writes; replacing the module retires existing observers.

These Bash startup assets target ALinux4 with its Bash 5.2; the same Hook behavior is also verified on Bash 5.1. Other Bash versions are not qualified. It checks an interactive Linux login terminal, required Shell capabilities, and the existing native prompt callback before registration. At the first primary prompt it rechecks the final Shell configuration and installs ERR only if compatible. Existing user traps or unknown prompt callbacks take priority. Later changes to callbacks, traps, options, or critical functions retire the observer without overwriting user configuration or installing it again. This checks observable runtime state; it does not inspect or claim compatibility with arbitrary initialization scripts.

The external owner still installs the profile and bashrc include separately. The independent login display can work when failure guidance is skipped. Test both normal activation and conflict retreat on the target image before rollout; successful protocol checks alone are not image-release acceptance.

## ECS automatic access

Reuse Core's existing ECS automatic-access flow on the first explicit question.
Core owns eligibility, activation/suppression and the `ecs_identity` model fixed
to `auto`; this integration does not seed a provider or a model into system or
user configuration. Existing explicit provider choices remain authoritative.
See the canonical [Core provider guide](../../../../docs/user-guide/en/cosh-ng/core/providers.md).
Real-provider and target-image qualification remain separate acceptance scopes.
