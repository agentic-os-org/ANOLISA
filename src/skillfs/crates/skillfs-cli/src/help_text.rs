//! Long CLI help text and help section labels.

pub const HEADING_MOUNT: &str = "Mount layout";
pub const HEADING_PROCESS: &str = "Process";
pub const HEADING_OBSERVABILITY: &str = "Observability";
pub const HEADING_SECURITY: &str = "Security and policy";
pub const HEADING_TRUSTED_WRITERS: &str = "Trusted writers";
pub const HEADING_CONFIG: &str = "Configuration";
pub const HEADING_LEDGER: &str = "Ledger integration";
pub const HEADING_TRUSTED_PEER: &str = "Trusted peer control";

pub const CLI_LONG_ABOUT: &str = r#"SkillFS exposes a curated set of agent skills through a FUSE filesystem.

A source directory contains one subdirectory per skill, each with a SKILL.md.
SkillFS mounts a /skills view for agents, keeps ordinary skill files on disk,
and can optionally enable audit, security policy, activation, and ledger
integration for production deployments."#;

pub const CLI_AFTER_HELP: &str = r#"Common workflows:
  skillfs classify ./skills --primary-count 6
      Generate skillfs-views.toml so agents see a smaller default skill set.

  skillfs mount ./skills /mnt/skillfs --foreground
      Mount the default /skills view for local testing.

  skillfs mount ./skills ./skills --security-mode --audit-log /var/log/skillfs/audit.jsonl
      Run an in-place mount so policy and audit cover source-path access.

Run 'skillfs <command> --help' for command-specific options and examples."#;

pub const MOUNT_LONG_ABOUT: &str = r#"Mount a SkillFS view.

Normal layout: SOURCE and MOUNTPOINT are different directories. Agents access
skills through MOUNTPOINT/skills, while direct writes to SOURCE bypass SkillFS.
This is useful for development and compatibility.

Security layout: SOURCE and MOUNTPOINT resolve to the same directory and
--security-mode is set. SkillFS over-mounts the source so normal userspace
access goes through FUSE policy and audit. Use this for production security
integration."#;

pub const MOUNT_AFTER_HELP: &str = r#"Examples:
  skillfs mount ./skills /mnt/skillfs --foreground
      Development mount. Browse skills at /mnt/skillfs/skills.

  skillfs mount ./skills /mnt/skillfs --managed
      Opt-in managed mount. A detached supervisor keeps the mount alive and
      remounts it if the FUSE worker exits, so a gateway restart does not drop
      the mount. Clear the desired state with 'skillfs stop /mnt/skillfs'.

  skillfs mount ./skills ./skills --security-mode --audit-log /var/log/skillfs/audit.jsonl
      In-place mount with audit events and .skill-meta protection.

  skillfs mount ./skills ./skills --security-mode --security --activation-mode file \
    --notify-socket /run/skill-ledger.sock \
    --ledger-backing-root /run/user/$UID/skillfs-ledger/source \
    --trusted-writer-exe /usr/bin/skill-ledger
      Ledger-driven activation flow. The daemon writes activation state;
      SkillFS exposes current, fallback, or hidden skill views. Put the
      daemon-facing source/backing root under /run/user/$UID/... or /run/...:
      agent-sec-core.service runs with PrivateTmp=true, so /tmp and /var/tmp
      are invisible to the daemon and are rejected at startup.

Important paths:
  SOURCE      Directory that stores real skill folders and skillfs-views.toml.
  MOUNTPOINT  Directory where SkillFS exposes /skills; identical to SOURCE
              only when using --security-mode.

Security notes:
  .skill-meta is readable but protected from ordinary mutation.
  Reserved lifecycle roots (.staging, .certified, .quarantine, .archive) are
  hidden from the ordinary view.
  Same-skill relative symlinks and same-skill hardlinks are allowed; cross-skill
  and absolute symlink targets are rejected by policy."#;

pub const STOP_AFTER_HELP: &str = r#"Example:
  skillfs stop /mnt/skillfs
      Clear the managed desired state, terminate the supervisor and worker,
      and unmount. Idempotent: safe to run when nothing is mounted.

Only managed mounts (started with 'skillfs mount --managed') are tracked. A
non-managed foreground mount is stopped with SIGTERM / Ctrl+C or fusermount3 -u."#;

pub const CLASSIFY_AFTER_HELP: &str = r#"Example:
  skillfs classify ./skills --primary-count 6

The generated skillfs-views.toml controls which skills appear in the default
agent view. Skills outside the default view can still be discovered through the
skill-discover virtual skill."#;
