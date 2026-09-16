//! OpenClaw framework driver.
//!
//! OpenClaw plugin adapters use the CLI-managed registry: `enable` runs
//! `openclaw plugins install <resource_root>` then `plugins enable <plugin_id>`;
//! `disable` runs `openclaw plugins uninstall <plugin_id>`. Skill-only adapters
//! (`adapter_type = "skill_bundle"`) skip registry operations and only
//! copy declared skills into the OpenClaw skills directory. Status is the
//! read-only `openclaw plugins list` for plugin adapters. All CLI and
//! filesystem operations go through the Manager's helpers — the driver
//! only builds argv arrays from validated data.
//!
//! An adapter may also declare bundled plugins it *displaces*
//! (`[adapters.openclaw].displaces`): plugins whose tool names collide with the
//! adapter's own and which OpenClaw's first-wins tool registry resolves by
//! dropping the adapter's tools. `enable` verifies each declared id against the
//! host's effective inventory and policy before any mutation — a contract naming
//! a plugin this host does not have fails there, rather than after this adapter's
//! own plugin is installed and verified — then disables the ones it may claim,
//! after its own plugin verifies loaded, and records the transition in the
//! receipt;
//! `disable` re-enables exactly what the receipt claims, and steps aside when
//! the exclusive slot the plugin would re-take has since been given to another
//! plugin or explicitly closed (`plugins.slots.<slot> = "none"`) — either way
//! `plugins enable` would re-run OpenClaw's slot selection and undo a choice
//! the operator made after `enable`. A plugin the operator had already turned off
//! before `enable` — by hand, or by `plugins.deny` / `plugins.enabled = false`,
//! which are the two settings OpenClaw both refuses to load a plugin under and
//! refuses a `plugins enable` against — is never claimed, so `disable` never
//! re-enables it; and one the host has since dropped or blocked by policy counts as
//! *released* rather than failed, because a restore that can never succeed would
//! otherwise strand the receipt forever with this adapter's own plugin already
//! removed. A restrictive `plugins.allow` is in neither list: it does not keep the
//! plugin from loading, so it is no reason to skip the hand-off, and it gates only
//! the restore — see `displacement_probe`.
//!
//! Displacement is this driver's contract only: the adapter bundle's own
//! `install.sh` / `uninstall.sh` script entry point does not read the
//! declaration and performs no hand-off.
//!
//! Re-enable inherits only the ownership the contract being enabled *now*
//! still declares, and only when the prior receipt was written against the same
//! state directory. A component upgrade that dropped the declaration, replaced
//! the plugin, or moved it to another slot has to take effect, and a fresh probe
//! cannot supply the fact (the plugin is already disabled by this adapter) — but
//! a different `OPENCLAW_STATE_DIR` is a different registry, where the operator
//! may have disabled the same plugin themselves, and inheriting would claim a
//! choice made in the old home on the strength of a probe that never ran there.
//! Which state directories a prior receipt may name at all is the Manager's trust
//! boundary, not this driver's; the driver only refuses to carry ownership
//! between the ones that survive it.
//! The slot is taken from the current declaration, not from history. Whatever is
//! not inherited is handed back by `cleanup_replaced_claim` — in full, from the
//! prior receipt's own home, when the migration crosses directories — while that
//! receipt is still the durable record of why the plugin was off.
//!
//! Both dry-runs read the receipt, not just the host, and a plan never promises
//! less than the operation does. A re-enable plan reports the displacement the
//! prior receipt carries over instead of what a fresh probe sees (by then the
//! plugin is disabled *by this adapter*, so a probe would promise the opposite
//! of the real lifecycle), and also lists the restore of a displacement the new
//! contract dropped — a mutation `plan_enable` cannot show, since it only walks
//! the current contract, but `cleanup_replaced_claim` really performs first. A
//! disable plan lists the restore a real disable performs, under the same
//! condition the real branch applies, and `validate_claim` rejects on the
//! dry-run path exactly the receipts the real disable would. Neither plan promises
//! more than its operation will do either: where the operation stops — on a hand-off
//! attribution the host cannot answer, the one question whose negative direction
//! destroys its own evidence — the plan stops on the same read, because a plan
//! describing a restore the operation then refuses to perform is the same lie told
//! earlier, and to somebody who is still deciding whether to run it. `status` verifies
//! the displacement too: a bundled plugin re-enabled behind this adapter's back
//! holds the tool names again while every other condition still reads clean, and
//! that reports `False`. A plugin merely *recorded* as disabled reports
//! `Unknown`, because `plugins disable` only writes config and when the running
//! gateway picks that up depends on the host's plugin reload mode — and this
//! driver has no channel to the running gateway, so it cannot tell whether the
//! change has been applied. Guessing "applied" there would make `status` the
//! false all-clear this condition exists to prevent. That verdict is also permanent, and says so: restarting the
//! gateway changes what OpenClaw serves but not what ANOLISA can observe, so the
//! reason points at an actual tool call for confirmation — the only check that
//! travels through the running gateway, named generically because `displaces` is
//! not any one component's contract — and deliberately not at
//! `plugins list` or `plugins inspect`, which read the same persisted state this
//! verdict came from and would hand back a false confirmation before a restart.
//! Nor does it imply a restart will turn `unknown` into `healthy`. Settling it
//! from here needs a gateway tool-catalog channel, which this driver does not
//! have.
//!
//! Every probe about a receipt's own state reads the instance that receipt
//! names — `claim_state_dir(claim)` — not the one the caller's environment
//! happens to point at. `OPENCLAW_HOME` and `OPENCLAW_STATE_DIR` can both have
//! moved since enable, and the Manager still validates such a receipt, so a
//! status that probed the caller's directory would report on a host this receipt
//! never touched: with `OPENCLAW_HOME=A` retained and `OPENCLAW_STATE_DIR=B`
//! overriding it, a B that happens to look clean hides a collision already
//! restored in A.
//!
//! A receipt whose references do not resolve — dangling, mistyped, aimed at
//! this adapter's own plugin or another framework's, duplicated, or naming an
//! empty or twice-claimed exclusive slot — is rejected before the first action
//! of `status` and `disable` alike, not part-way through: `Healthy` would be a
//! report about a plugin the driver cannot name, and a `disable` that noticed
//! only at restore time would already have uninstalled the adapter's own plugin.
//! The slot rules the Manager applies to a contract are re-applied to the
//! receipt, because the receipt is what disable consumes and it can be edited
//! without the contract ever being re-read. For the same reason the driver
//! resolves that own plugin through `OpenClawClaim.plugin_resource` instead of
//! taking the first `FrameworkPlugin` in the resource list — a receipt may
//! legitimately carry two, and their order is nobody's promise.
//!
//! The CLI env contract mirrors `openclaw/scripts/install.sh`: unset
//! `OPENCLAW_HOME`, set `OPENCLAW_STATE_DIR` to the resolved state directory,
//! and prepend the standard bin dirs to `PATH`. `OPENCLAW_BIN` overrides
//! the executable (used by tests to point at a fake CLI).
//!
//! Before the first framework mutation — during `prepare_enable` (and, for
//! `--dry-run`, `plan_enable`) — `enable` builds a read-only
//! `OpenClawHostProfile` from read-only probes (`openclaw --version`,
//! `plugins install --help`, `plugins enable --help`, `plugins inspect --help`).
//! From it the driver gates on the adapter's declared framework version, chooses
//! version-conditioned config, requires install `--force`, and accepts declared
//! capabilities when each subcommand advertises `--accept-capabilities`. It adds
//! `--dangerously-force-unsafe-install` only when both authorized and
//! advertised as effective by the host, and records the inspect capabilities.
//! The install/verify capabilities flow to `apply_enable` as typed
//! [`PreparedEnable`] state, so
//! apply performs no probe of its own — each probe runs exactly once per
//! enable, all before the first mutation. The host version or argv is never
//! written into the receipt — the receipt stays pure typed data.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    ConfigApplyState, DRIVER_SCHEMA_VERSION, DisplacedPluginRef, DriverPayload, OpenClawClaim,
    validate_config_key, validate_plugin_id,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimProgress, ClaimResourceRef, CliOutput, ConditionStatus, DetectResult, DisableReport,
    DriverCtx, DriverPlan, FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable,
    find_binary_in_path,
};
use super::managed_files::{MaterializedMapping, copy_materialized_resource};
use crate::manifest::{AdapterConfigSetSpec, DisplacedPluginSpec};

/// Default timeout for an OpenClaw CLI invocation.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Resource ids used in OpenClaw receipts. Stable strings referenced from
/// the [`OpenClawClaim`] payload and condition reports.
const RES_STATE_DIR: &str = "openclaw_state_dir";
const RES_PLUGIN: &str = "openclaw_plugin";

/// [`ClaimResource::purpose`] of this adapter's own framework plugin. Checked
/// when resolving it, so a receipt that points `plugin_resource` at some other
/// resource is rejected instead of silently acted on.
const PURPOSE_PLUGIN: &str = "openclaw_plugin";

/// [`ClaimResource::purpose`] of a framework plugin this adapter displaced.
/// Distinguishes a displaced bundled plugin from [`RES_PLUGIN`], the adapter's
/// own registration — both are [`ClaimResourceKind::FrameworkPlugin`]
/// resources on the same framework.
const PURPOSE_DISPLACED_PLUGIN: &str = "openclaw_displaced_plugin";

/// Resource-id prefix for a displaced framework plugin. The id embeds the
/// plugin id so one receipt can displace several plugins unambiguously.
const RES_DISPLACED_PREFIX: &str = "openclaw_displaced_plugin_";

/// OpenClaw driver. Stateless; all per-operation context arrives via
/// [`DriverCtx`].
pub struct OpenClawDriver;

impl OpenClawDriver {
    /// Construct the driver.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenClawDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for OpenClawDriver {
    fn name(&self) -> &'static str {
        "openclaw"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        match find_binary_in_path(&openclaw_bin()) {
            Some(path) => DetectResult {
                detected: true,
                reason: format!("openclaw CLI found at {}", path.display()),
            },
            None => {
                // The CLI is what enable/disable need; a bare home dir is
                // not sufficient. Report not-detected but mention the home
                // so a user understands the framework is partially present.
                let home_note = openclaw_home(env.user_home.as_deref())
                    .filter(|h| h.exists())
                    .map(|h| format!(" (home {} exists but CLI is not on PATH)", h.display()))
                    .unwrap_or_default();
                DetectResult {
                    detected: false,
                    reason: format!("openclaw CLI not found on PATH{home_note}"),
                }
            }
        }
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        openclaw_allowed_roots(ctx.user_home.as_deref())
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        let root = &ctx.resource_root;
        if !root.is_dir() {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root does not exist or is not a directory".to_string(),
            });
        }
        let is_empty = root
            .read_dir()
            .map_err(|source| AdapterError::Io {
                path: root.clone(),
                source,
            })?
            .next()
            .is_none();
        if is_empty {
            return Err(AdapterError::BundleInvalid {
                root: root.clone(),
                reason: "resource root is empty".to_string(),
            });
        }

        let plugin_id = if ctx.is_skill_bundle() {
            None
        } else {
            let manifest_file = ctx
                .declared_bundle_entry
                .as_deref()
                .unwrap_or("openclaw.plugin.json");
            ctx.declared_plugin_id
                .clone()
                .or(read_plugin_manifest_id(root, manifest_file)?)
                .or_else(|| Some(ctx.component.clone()))
        };

        Ok(AdapterBundle {
            resource_root: root.clone(),
            plugin_id,
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        prior: Option<&AdapterClaim>,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let home = require_home(ctx)?;
        let mut actions = Vec::new();
        let mut register_command = None;
        let mut enable_command = None;
        // Plugin adapters resolve a read-only host profile so the dry-run
        // plan shows the exact install command (including whether the
        // authorized unsafe flag is used) and only the config the host
        // version actually selects — the same decisions a real enable makes.
        let mut selected_config: Vec<(usize, &AdapterConfigSetSpec)> = Vec::new();
        if ctx.is_skill_bundle() {
            // The adapter-level version gate still applies to skill bundles.
            self.gate_skill_bundle_version(ctx)?;
        } else {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            let preflight = self.plugin_preflight(&bundle.resource_root, ctx)?;
            enable_command = Some(display_command(&build_enable_cmd(
                &plugin_id,
                &home,
                ctx.user_home.as_deref(),
                preflight.supports_enable_accept_capabilities,
            )));
            selected_config = preflight.selected_config;
            register_command = Some(display_command(&preflight.install_cmd));
            actions.push(format!(
                "register openclaw plugin '{plugin_id}' from {}",
                bundle.resource_root.display()
            ));
        }

        for skill in &ctx.declared_skills {
            let src_display = match skill.source {
                Some(ref s) => s.display().to_string(),
                None => format!("{}/skills/{}", bundle.resource_root.display(), skill.name,),
            };
            actions.push(format!(
                "deliver openclaw skill '{}' from {} to {}/skills/{}",
                skill.name,
                src_display,
                home.display(),
                skill.name,
            ));
        }
        for &(i, cfg) in &selected_config {
            actions.push(format!(
                "set openclaw config [{i}] {} = {}",
                cfg.key,
                config_value_display(&cfg.value)
            ));
        }

        if let Some(command) = enable_command {
            actions.push(format!("enable openclaw plugin: {command}"));
        }

        // Read-only probe, so a dry run reports the same hand-off a real enable
        // would perform — including the "operator already disabled it" case,
        // which claims nothing and therefore plans no mutation.
        //
        // A re-enable is the exception, and the receipt overrides the probe
        // there: the plugin is disabled *by this adapter*, so
        // `plugins.entries.<id>.enabled` reads `false` and a fresh probe would
        // plan "leave it alone, disable will not re-enable it" — the exact
        // opposite of what `preserve_reenable_facts` carries over and a later
        // `disable` then does. Plan those from the prior receipt instead.
        if !ctx.is_skill_bundle() {
            // Carry-over only describes a receipt written against *this* state
            // directory; across a migration the plan has to show what a fresh
            // probe of the new home finds, exactly as `preserve_reenable_facts`
            // inherits nothing and `cleanup_replaced_claim` restores the old home
            // in full.
            let carried = match prior {
                Some(prior) if claim_state_dir(prior)? == home => {
                    // Applied entries only, matching what
                    // `preserve_openclaw_displaced_facts` will actually carry and
                    // what a later `disable` will therefore restore. Promising a
                    // carry-over for an entry whose hand-off never ran previews a
                    // restore the real disable declines to perform.
                    claim_applied_displacement_ids(prior)?
                }
                _ => Vec::new(),
            };
            // The other half of what a re-enable carries over, asked from the same
            // inputs and the same predicate `prepare_enable` asks: an *unapplied*
            // entry of a prior receipt for this instance, which the replacement
            // receipt still takes ownership of when the host shows this adapter's
            // own slot selection having performed the hand-off. Empty on a first
            // enable and across a migration, exactly as it is there.
            let unrecorded_prior_handoffs = prior_unrecorded_displacements(prior, ctx)?;
            let inventory = (!ctx.declared_displaces.is_empty())
                .then(|| self.read_plugin_inventory(&home, ctx))
                .flatten();
            for spec in &ctx.declared_displaces {
                validate_plugin_id(&spec.id)?;
                // The restore a later `disable` would perform, described by the
                // one function that enumerates its vetoes. A preview naming a
                // different condition than the real path applies is worse than no
                // preview at all: the operator plans around it, and because every
                // veto still counts as completed cleanup the receipt is removed
                // afterwards and nothing records the divergence.
                let slot_note = restore_conditions_note(spec.slot.as_deref());
                // Existence is checked for a carried-over id too, and from the
                // inventory already read above, so this costs no extra host call.
                // See [`inventory_omits_plugin`].
                if inventory_omits_plugin(inventory.as_deref(), &spec.id) {
                    return Err(missing_displacement_target(ctx, &spec.id));
                }
                // A carried id is probed like any other. Skipping the probe here
                // is what let the plan and the real enable diverge a second time:
                // `prepare_enable` always probes, so when the host says the plugin
                // is positively *on* the real enable claims it fresh and disables
                // it again — while a plan that only checked ownership kept promising
                // "it stays claimed", hiding a host mutation that overrides what the
                // operator just chose. Ownership is carried over only when the host
                // does not positively contradict it.
                let probe = self.displacement_probe(spec, inventory.as_deref(), &home, ctx);
                let carries_over =
                    carried.contains(&spec.id) && !matches!(probe, DisplacementProbe::ClaimEnabled);
                // A positive `false` is the one probe answer that says nothing about
                // *who* turned the plugin off, so the plan asks the recovery
                // `prepare_enable` asks before it promises anything about that
                // ownership. Previewing "leave it alone, disable will not re-enable
                // it" over an enable whose replacement receipt carries the hand-off
                // as performed — and whose disable therefore really does re-enable
                // it — hands the operator the opposite of the answer they asked for.
                let recovers_prior_handoff = matches!(probe, DisplacementProbe::FlagDisabled)
                    && matches!(
                        self.recovered_prior_handoff(
                            &unrecorded_prior_handoffs,
                            &spec.id,
                            bundle.plugin_id.as_deref(),
                            &home,
                            ctx,
                        )?,
                        PriorHandoffRecovery::Recovered
                    );
                if carries_over {
                    actions.push(format!(
                        "keep openclaw plugin '{}' disabled: the receipt being replaced already \
                         claims this displacement and carries it over, so it stays claimed and \
                         disable will hand it back{slot_note}",
                        spec.id
                    ));
                } else {
                    match probe {
                        // Named separately from an ordinary claim because this one
                        // undoes something the operator did after the last enable,
                        // which is exactly what a preview must not bury.
                        DisplacementProbe::ClaimEnabled if carried.contains(&spec.id) => actions
                            .push(format!(
                                "disable openclaw plugin '{}' again: it was re-enabled after the \
                                 receipt being replaced displaced it, so this enable takes the \
                                 tool names back{slot_note}",
                                spec.id
                            )),
                        DisplacementProbe::ClaimEnabled | DisplacementProbe::ClaimUnverified => {
                            actions.push(format!(
                                "disable openclaw plugin '{}' so it releases the tool names this \
                                 adapter registers{slot_note}",
                                spec.id
                            ))
                        }
                        DisplacementProbe::AlreadyOff(reason) => actions.push(format!(
                            "leave openclaw plugin '{}' alone ({reason}, so it is not claimed \
                             and disable will not re-enable it)",
                            spec.id
                        )),
                        // The recovery `prepare_enable` performs, said out loud. No
                        // host mutation is planned for it — the hand-off already ran,
                        // so all this enable does is carry the ownership forward —
                        // but that ownership is exactly what a later `disable` acts
                        // on, and the unqualified "leave it alone" arm below denies
                        // it.
                        DisplacementProbe::FlagDisabled if recovers_prior_handoff => {
                            actions.push(format!(
                                "keep openclaw plugin '{}' disabled: the receipt being replaced \
                                 displaced it without marking the hand-off, and the host shows \
                                 this adapter's own slot selection having performed it, so this \
                                 enable carries that ownership over and disable will hand it \
                                 back{slot_note}",
                                spec.id
                            ))
                        }
                        DisplacementProbe::FlagDisabled => actions.push(format!(
                            "leave openclaw plugin '{}' alone (already disabled before this \
                             adapter, so it is not claimed and disable will not re-enable it)",
                            spec.id
                        )),
                        DisplacementProbe::NotOnHost => {
                            return Err(missing_displacement_target(ctx, &spec.id));
                        }
                    }
                }
            }
        }

        Ok(DriverPlan {
            framework: self.name().to_string(),
            component: ctx.component.clone(),
            actions,
            register_command,
        })
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let prior_home = claim_state_dir(prior)?;
        if prior_home == require_home(ctx)? {
            // The prior installation continues, so there is nothing to
            // unregister — but a displacement the contract being enabled no
            // longer declares does not continue, and `cleanup_replaced_claim`
            // really does re-enable it before the receipts swap. `plan_enable`
            // only walks the *current* contract, so without this a dry-run would
            // show no trace of a host mutation the real run performs first.
            return self.plan_dropped_displacement_restores(prior, ctx);
        }

        let mut actions = Vec::new();
        let prior_own_plugin = claim_own_plugin(prior)?;
        if let Some(plugin_id) = &prior_own_plugin {
            validate_plugin_id(plugin_id)?;
            actions.push(format!(
                "unregister prior openclaw plugin '{plugin_id}' from {}",
                prior_home.display()
            ));
        }
        for skill_name in claim_skill_resources(prior) {
            actions.push(format!(
                "remove prior openclaw skill '{skill_name}' from {}",
                prior_home.join("skills").join(&skill_name).display()
            ));
        }
        // A cross-home cleanup runs a full `disable` on the prior receipt, which
        // also hands back every plugin it displaced — in the *old* home, where
        // this adapter is what turned them off. Nothing else in the plan walks
        // the prior receipt's displacements, so without this a migration dry-run
        // would omit the restore it is about to perform. The verdict comes from
        // the same `restore_decision` the real restore calls, read against that
        // prior home, so this preview cannot drift from the operation either.
        let prior_displaced = claim_displaced_plugins(prior)?;
        // That `disable` also inherits the attribution gate the real disable gained,
        // so this plan has to ask it too — against the *prior* home, which is the one
        // the operation reads and not the one `ctx` resolves to. Without it the
        // migration dry-run described a prior-home restore the migration then refused
        // to perform, which is the divergence the shared gate exists to rule out and
        // the reason it takes the receipt rather than resolving a home of its own.
        self.gate_disable_restores_on_attribution(prior, &prior_displaced, ctx)?;
        actions.extend(self.restore_preview_lines(
            prior,
            &prior_displaced,
            ctx,
            &format!(" in the prior state directory {}", prior_home.display()),
            "which the prior receipt displaced",
            // That `disable` unregisters the prior plugin first, so the allowlist
            // the restore reads is the one that uninstall leaves behind.
            AllowlistReading::AfterOwnUninstall(prior_own_plugin.as_deref()),
        )?);
        Ok(actions)
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        prior: Option<&AdapterClaim>,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let home = require_home(ctx)?;

        // Resolve the plugin id and, for plugin adapters, run all read-only
        // probing and gating BEFORE building any resource — so a version
        // mismatch, a missing `--force`/`--json`, or an unsupported authorized
        // unsafe flag fails here, before the Manager persists the receipt. The
        // config the host version selects is computed once and carried as
        // transient intent; apply journals each entry as Pending before its
        // write and confirms it only after success. The install/verify
        // capabilities are carried forward to `apply_enable` as typed
        // `PreparedEnable` so apply never re-probes.
        let plugin_id = if ctx.is_skill_bundle() {
            None
        } else {
            let plugin_id = require_plugin_id(bundle)?;
            validate_plugin_id(&plugin_id)?;
            Some(plugin_id)
        };
        let mut prepared = if ctx.is_skill_bundle() {
            // Skill bundles run no plugin install, but the adapter-level
            // version gate still applies before the receipt is persisted.
            self.gate_skill_bundle_version(ctx)?;
            PreparedEnable::None
        } else {
            let preflight = self.plugin_preflight(&bundle.resource_root, ctx)?;
            PreparedEnable::OpenClaw {
                supports_accept_capabilities: preflight.supports_accept_capabilities,
                supports_enable_accept_capabilities: preflight.supports_enable_accept_capabilities,
                supports_unsafe_install: preflight.supports_unsafe_install,
                supports_inspect_json: preflight.supports_inspect_json,
                supports_inspect_runtime: preflight.supports_inspect_runtime,
                selected_config_indices: preflight
                    .selected_config
                    .iter()
                    .map(|(index, _)| *index)
                    .collect(),
                // Filled in below, once the displacement probe has run.
                freshly_claimed_displacements: Vec::new(),
            }
        };

        let mut resources = vec![ClaimResource {
            id: RES_STATE_DIR.to_string(),
            purpose: "openclaw_state_dir".to_string(),
            kind: ClaimResourceKind::ExternalPath { path: home.clone() },
        }];
        if let Some(plugin_id) = &plugin_id {
            resources.push(ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: PURPOSE_PLUGIN.to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: self.name().to_string(),
                    plugin_id: plugin_id.clone(),
                },
            });
        }

        // Framework plugins this adapter displaces (#3225). Probed here, in
        // prepare, because the answer decides what the receipt may *claim*: a
        // plugin the operator already disabled themselves is their choice, and
        // recording it would make a later `adapter disable` re-enable a plugin
        // they deliberately turned off. Only a positive `false` is honored as
        // that choice — an absent key means "bundled default" (enabled), and so
        // does a probe this host cannot answer, because skipping the claim
        // there strands the host with nothing behind the slot after disable,
        // whereas an unwanted restore costs one `plugins disable`.
        //
        // This is not the last word: `apply_displacements` re-confirms each claim
        // immediately before it mutates, because install, config and runtime
        // verification all run in between and the host can change during them.
        // Which claims it may re-confirm is decided here, per claim, and turns on
        // whether an older receipt already owns the transition.
        let mut displaced_plugins = Vec::new();
        let mut freshly_claimed_displacements: Vec<String> = Vec::new();
        if plugin_id.is_some() {
            let inventory = (!ctx.declared_displaces.is_empty())
                .then(|| self.read_plugin_inventory(&home, ctx))
                .flatten();
            let inherited_displacements = inherited_displacement_ids(prior, ctx)?;
            let unrecorded_prior_handoffs = prior_unrecorded_displacements(prior, ctx)?;
            for spec in &ctx.declared_displaces {
                // The Manager validates declared ids before a driver sees
                // them; re-check because `DriverCtx` is public and this id is
                // about to enter an argv.
                validate_plugin_id(&spec.id)?;
                // Whether the receipt entry written below may say the hand-off has
                // already been performed. Exactly one branch can: an unverified
                // claim that a prior receipt for this same instance already owns,
                // i.e. the one case where this enable carries an older ownership
                // forward rather than establishing one of its own.
                //
                // Decided here and nowhere else, because this is the only scope
                // that holds both halves of the question — the prior receipt and
                // what this round's probe concluded about it. `preserve_reenable_facts`
                // sees the receipt but not `PreparedEnable`, and the receipt
                // carries no transient attribution on purpose, so a merge there can
                // only key on the resource match. That is wrong in both directions:
                // it drops a prior hand-off this enable is carrying forward, and it
                // promotes a prior hand-off this enable's own *positive* probe has
                // just invalidated — the operator re-enabled the plugin, so the old
                // ownership is void and this round's claim is fresh and unperformed.
                let carries_prior_handoff =
                    match self.displacement_probe(spec, inventory.as_deref(), &home, ctx) {
                        // Positively on, so the transition about to happen is this
                        // enable's own and `apply_displacements` may re-confirm it.
                        DisplacementProbe::ClaimEnabled => {
                            freshly_claimed_displacements.push(spec.id.clone());
                            false
                        }
                        // Claimed, but the host could not say whether the plugin was
                        // on — so who a later `false` belongs to has to come from the
                        // prior receipt, and the answer differs by whether there is
                        // one:
                        //
                        // - This instance already has a receipt owning the id, and
                        //   that ownership is about to be carried into the replacement
                        //   (`preserve_openclaw_displaced_facts` declines to re-add it
                        //   only because this fresh claim occupies the same resource).
                        //   A `false` at apply time is then at least as likely to be
                        //   *that* receipt's own disable as somebody else's doing, and
                        //   re-confirming would delete ownership this receipt is the
                        //   only remaining record of.
                        // - Nothing inherits it: a first enable, or a prior written
                        //   against another state directory. There is no older
                        //   ownership to protect, so the same `false` can only mean
                        //   the plugin went off for a reason this enable never
                        //   observed. Re-confirm and release, or the receipt claims a
                        //   transition it did not make and a later disable re-opens a
                        //   plugin the operator closed themselves — the takeover
                        //   window, reachable through a transient probe failure.
                        DisplacementProbe::ClaimUnverified => {
                            if inherited_displacements.contains(&spec.id) {
                                true
                            } else {
                                freshly_claimed_displacements.push(spec.id.clone());
                                false
                            }
                        }
                        // Not this adapter's transition to undo: a policy that
                        // keeps the plugin off and would also refuse the restore.
                        DisplacementProbe::AlreadyOff(_) => continue,
                        // A positive `false` is the one probe answer that says
                        // nothing about *who* turned the plugin off — see the
                        // variant — so it cannot settle ownership on its own, and
                        // the host may still hold the corroborating half of this
                        // adapter's own slot selection. Ask the predicate the three
                        // existing recovery marks ask, and carry the hand-off into
                        // the replacement receipt as already performed when it
                        // answers yes.
                        //
                        // Skipping it here lost the hand-off outright, and the loss
                        // survived a *successful* retry. Nothing else inherits an
                        // unapplied entry: `preserve_openclaw_displaced_facts`
                        // declines to, so `dropped_displaced_plugins` picks it up and
                        // the same-home cleanup hands the plugin back — but this
                        // enable's own install and enable then switch it off again
                        // through the very slot selection the recovery just
                        // attributed, over a replacement receipt with no entry for
                        // it. The re-enable succeeded, the disable that followed
                        // reported an unregistered plugin and removed the receipt,
                        // and the bundled backend stayed off with no operator
                        // anywhere in the sequence and nothing left recording why.
                        //
                        // Positive evidence only, and only for a plugin a prior
                        // receipt for this same instance already names. Inheriting
                        // *every* unapplied entry would claim an operator's own
                        // disable, which is what this branch exists to avoid;
                        // claiming on the slot reading alone reaches the same
                        // mistake on a first enable, where there is no older record
                        // to recover and a slot naming this adapter's plugin can
                        // mean nothing more than that an operator installed it by
                        // hand and closed the bundled one themselves.
                        //
                        // "No positive evidence" is two answers, and only one of them
                        // may decline the recovery. A readable host that rules this
                        // adapter out really is somebody else's disable, so the
                        // `false` belongs to whoever wrote it and this enable claims
                        // nothing. A host that could not answer is not that: the prior
                        // receipt is still the only record of a hand-off this adapter
                        // may well have performed, and the steps that follow act on
                        // its absence — same-home cleanup hands the plugin back, this
                        // enable's own install switches it off again through the very
                        // selection the recovery was being asked about, and the
                        // replacement receipt names nothing to restore. So a read
                        // failure fails the enable here, before the first mutation and
                        // with that receipt left intact for the retry. See
                        // [`OpenClawDriver::recovered_prior_handoff`].
                        DisplacementProbe::FlagDisabled => {
                            match self.recovered_prior_handoff(
                                &unrecorded_prior_handoffs,
                                &spec.id,
                                plugin_id.as_deref(),
                                &home,
                                ctx,
                            )? {
                                PriorHandoffRecovery::Recovered => true,
                                PriorHandoffRecovery::NothingToRecover => continue,
                            }
                        }
                        // A contract naming a plugin this host does not have is a
                        // broken contract, and this is still before the first
                        // mutation — installing first and letting `plugins disable`
                        // error afterwards would leave a receipt whose restore could
                        // never converge either.
                        DisplacementProbe::NotOnHost => {
                            return Err(missing_displacement_target(ctx, &spec.id));
                        }
                    };
                let resource_id = displaced_resource_id(&spec.id);
                resources.push(ClaimResource {
                    id: resource_id.clone(),
                    purpose: PURPOSE_DISPLACED_PLUGIN.to_string(),
                    kind: ClaimResourceKind::FrameworkPlugin {
                        framework: self.name().to_string(),
                        plugin_id: spec.id.clone(),
                    },
                });
                displaced_plugins.push(DisplacedPluginRef {
                    resource: resource_id,
                    slot: spec.slot.clone(),
                    // False unless this entry inherits a hand-off a prior receipt
                    // already performed. This enable's own hand-off has not run —
                    // it cannot, because this is still before the first mutation
                    // and `plugins disable` only runs once this adapter's own
                    // plugin is verified loaded — so `apply_displacements` marks it
                    // there, and everything that would act on the ownership checks
                    // the mark first.
                    applied: carries_prior_handoff,
                });
            }
        }

        let mut skill_resources = Vec::new();
        for skill in &ctx.declared_skills {
            let res_id = format!("openclaw_skill_{}", skill.name);
            resources.push(ClaimResource {
                id: res_id.clone(),
                purpose: "openclaw_skill".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: home.join("skills").join(&skill.name),
                },
            });
            skill_resources.push(res_id);
        }

        if let PreparedEnable::OpenClaw {
            freshly_claimed_displacements: slot,
            ..
        } = &mut prepared
        {
            *slot = freshly_claimed_displacements;
        }

        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: ctx.component.clone(),
            framework: self.name().to_string(),
            plugin_id,
            adapter_type: ctx.adapter_type.clone(),
            enabled_at: now_iso8601(),
            resource_root: bundle.resource_root.clone(),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources,
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: RES_STATE_DIR.to_string(),
                plugin_resource: if ctx.is_skill_bundle() {
                    String::new()
                } else {
                    RES_PLUGIN.to_string()
                },
                skill_resources,
                // Pending/applied config resources are journaled during apply;
                // this list references confirmed entries only.
                config_resources: Vec::new(),
                displaced_plugins,
            }),
        };
        Ok((claim, prepared))
    }

    fn plan_disable_restores(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let displaced = claim_displaced_plugins(claim)?;
        if displaced.is_empty() {
            return Ok(Vec::new());
        }
        // A real disable uninstalls this adapter's own plugin before it restores
        // anything, and that uninstall is what rewrites `plugins.allow`.
        let own_plugin_id = claim_own_plugin(claim)?;
        // Ask the operation's own attribution question before describing what it
        // would restore, because a real disable now fails on an answer the host
        // cannot give: a preview promising "left alone, this adapter never disabled
        // it" over a command that refuses to run describes a disable that does not
        // exist.
        self.gate_disable_restores_on_attribution(claim, &displaced, ctx)?;
        self.restore_preview_lines(
            claim,
            &displaced,
            ctx,
            "",
            "which this adapter displaced",
            AllowlistReading::AfterOwnUninstall(own_plugin_id.as_deref()),
        )
    }

    fn validate_claim(&self, claim: &AdapterClaim) -> Result<(), AdapterError> {
        // Resolve the receipt's own-plugin reference and every displaced-plugin
        // reference, and let either fail the call. `disable` and `status` both
        // resolve them before their first action anyway; resolving them here too
        // is what makes the dry-run reject the same receipts the real run does,
        // instead of handing back a plan for a disable that cannot happen.
        claim_own_plugin(claim)?;
        claim_displaced_plugins(claim).map(|_| ())
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        next: &mut AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<(), AdapterError> {
        preserve_openclaw_config_facts(prior, next)?;
        preserve_openclaw_displaced_facts(prior, next, ctx)
    }

    fn materialized_mappings(
        &self,
        resource_root: &Path,
        _adapter_type: Option<&str>,
        declared_skills: &[super::driver::DeclaredSkill],
    ) -> Vec<MaterializedMapping> {
        declared_skills
            .iter()
            .map(|skill| MaterializedMapping {
                resource_id: format!("openclaw_skill_{}", skill.name),
                source_root: skill
                    .source
                    .clone()
                    .unwrap_or_else(|| resource_root.join("skills").join(&skill.name)),
                excluded_prefixes: Vec::new(),
            })
            .collect()
    }

    fn materialized_destination_roots(
        &self,
        _bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<BTreeMap<String, PathBuf>, AdapterError> {
        let home = require_home(ctx)?;
        Ok(ctx
            .declared_skills
            .iter()
            .map(|skill| {
                (
                    format!("openclaw_skill_{}", skill.name),
                    home.join("skills").join(&skill.name),
                )
            })
            .collect())
    }

    fn materialized_verification_applicable(&self, claim: &AdapterClaim) -> bool {
        matches!(
            &claim.driver_payload,
            DriverPayload::OpenClaw(payload) if !payload.skill_resources.is_empty()
        )
    }

    fn cleanup_replaced_claim(
        &self,
        prior: &mut AdapterClaim,
        next: &AdapterClaim,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<DisableReport, AdapterError> {
        let prior_home = claim_state_dir(prior)?;
        if prior_home != claim_state_dir(next)? {
            // A state-directory migration creates a separate OpenClaw registry
            // and skills tree. Release the prior installation while its validated
            // receipt is still durable, before the Manager replaces ownership.
            // `disable` already hands back every displacement that receipt
            // claims, so a declaration the new contract dropped is covered here
            // too and needs no extra step. Reachable only for the migrations the
            // Manager's trust boundary admits; see
            // `preserve_openclaw_displaced_facts`.
            return self.disable(prior, ctx, progress);
        }

        // Same home, so the prior installation continues — but a displacement
        // the replacement receipt no longer claims does not, and this is the last
        // moment anybody can act on it. Once the Manager swaps the receipts the
        // prior record is gone, and a plugin it no longer names would stay
        // disabled with nothing recording that this adapter was what turned it
        // off. A failed restore reports `cleanup_complete = false`, which fails
        // the re-enable and leaves the validated prior receipt durable.
        //
        // The converse is the Manager's responsibility, and this hook cannot take
        // it on: `plugins enable` below really changes the host, while the record
        // that the ownership it undid is gone lives in a receipt this hook can only
        // reach through the mutable `prior` it is handed — which is why the Manager
        // persists `prior` before reporting an incomplete cleanup, and why
        // `restore_displaced_plugins` strikes each entry as its restore succeeds.
        // The same invariant is what `disable` owes on its own path. So a *successful* restore depends on the
        // Manager making the receipt swap its next durable action, with nothing
        // fallible in between — which is why the Manager prunes stale materialized
        // output before calling this hook rather than after. A prune failure used
        // to sit exactly there, leaving the host restored and the prior receipt
        // still claiming `applied` ownership of a plugin it had just handed back.
        let dropped = dropped_displaced_plugins(prior, next)?;
        if dropped.is_empty() {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: Vec::new(),
            });
        }
        // Nothing here mutates the host before the restore — a same-home
        // re-enable keeps the prior installation, and this hook runs after it —
        // so the live host is the one the decision should read.
        //
        // Ask the attribution before the restore rather than letting the restore
        // decide it, because the restore's own `SkipNotApplied` folds "the host
        // could not say" into "this adapter never did" — right for a `disable`,
        // whose receipt survives the skip and gets asked again, and wrong here,
        // where the receipt that would be asked again is the one this hook's
        // `Ok` lets the Manager replace.
        self.attribute_dropped_prior_handoffs(prior, &dropped, ctx)?;
        self.restore_displaced_plugins(prior, &dropped, &prior_home, ctx, SlotReadings::Live)
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        prepared: &PreparedEnable,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<(), AdapterError> {
        let home = require_home(ctx)?;
        let user_home = ctx.user_home.as_deref();

        // The install/verify capabilities (unsafe support, inspect `--runtime`)
        // and all gating were resolved and validated by `prepare_enable`, which
        // probed the host once and handed the results forward as `prepared`.
        // `apply_enable` therefore does NOT probe at all: the install argv is
        // rebuilt from the typed `ctx` and prepared consent support; the unsafe
        // flag still requires explicit authorization. Config selection comes from
        // prepared state, and runtime verification uses the prepared
        // `--runtime` capability. Each probe (`--version`, install/enable/inspect
        // `--help`) thus runs exactly once per enable, all in prepare,
        // and no two probe generations are ever mixed.
        //
        // Defense in depth: `PreparedEnable` and this trait are public, so a
        // caller could hand a mismatched value. Validate the (adapter kind,
        // prepared variant) pairing and re-check the capabilities BEFORE the
        // first mutation, failing closed on any mismatch rather than silently
        // degrading (which could skip the `--json` precondition, verify without
        // `--runtime`, or add the unsafe flag on an unverified host).
        let (
            accept_capabilities,
            enable_accept_capabilities,
            host_supports_unsafe,
            verify_with_runtime,
            selected_config_indices,
            freshly_claimed_displacements,
        ) = if ctx.is_skill_bundle() {
            if !matches!(prepared, PreparedEnable::None) {
                return Err(prepared_state_mismatch(
                    "skill_bundle adapters carry no prepared host capabilities",
                ));
            }
            // Skill bundles run no plugin install and no runtime verification;
            // these values are unused for them.
            (false, false, false, false, Vec::new(), Vec::new())
        } else {
            match prepared {
                PreparedEnable::OpenClaw {
                    supports_accept_capabilities,
                    supports_enable_accept_capabilities,
                    supports_unsafe_install,
                    supports_inspect_json,
                    supports_inspect_runtime,
                    selected_config_indices,
                    freshly_claimed_displacements,
                } => {
                    if !supports_inspect_json {
                        return Err(AdapterError::FrameworkCli {
                            program: openclaw_bin(),
                            reason: "`openclaw plugins inspect --help` does not expose --json; \
                                     cannot verify plugin runtime status"
                                .to_string(),
                        });
                    }
                    if ctx.allow_unsafe_plugin_install && !supports_unsafe_install {
                        return Err(AdapterError::FrameworkCli {
                            program: openclaw_bin(),
                            reason: "unsafe plugin install was explicitly authorized but this \
                                     openclaw does not expose --dangerously-force-unsafe-install"
                                .to_string(),
                        });
                    }
                    validate_prepared_config_indices(selected_config_indices, ctx)?;
                    (
                        *supports_accept_capabilities,
                        *supports_enable_accept_capabilities,
                        *supports_unsafe_install,
                        *supports_inspect_runtime,
                        selected_config_indices.clone(),
                        freshly_claimed_displacements.clone(),
                    )
                }
                PreparedEnable::None => {
                    return Err(prepared_state_mismatch(
                        "openclaw plugin enable requires prepared host capabilities",
                    ));
                }
                PreparedEnable::QoderNative { .. } => {
                    return Err(prepared_state_mismatch(
                        "openclaw plugin enable received Qoder capabilities",
                    ));
                }
            }
        };
        validate_config_claim_state(claim)?;
        validate_pending_config_selection(claim, &selected_config_indices, ctx)?;

        let plugin = if ctx.is_skill_bundle() {
            None
        } else {
            let plugin_id =
                claim_own_plugin(claim)?.ok_or_else(|| AdapterError::BundleInvalid {
                    root: claim.resource_root.clone(),
                    reason: "openclaw receipt has no plugin id".to_string(),
                })?;
            validate_plugin_id(&plugin_id)?;
            let cmd = base_cmd(
                install_argv(
                    &claim.resource_root,
                    ctx.allow_unsafe_plugin_install,
                    accept_capabilities,
                ),
                &home,
                user_home,
            );
            let program = cmd.program.clone();
            let output = ctx.ops.run_framework_cli(cmd)?;
            if !output.success() {
                let mut reason = full_failure_reason("plugins install", &output);
                // Point the operator at the explicit, auditable retry only when
                // it could actually help: the host exposes the unsafe flag, the
                // user did not already authorize it, and the failure looks like
                // a plugin-safety rejection. Never retry automatically.
                if install_output_requires_capability_consent(&output) {
                    reason.push_str(
                        "; OpenClaw capability consent was not accepted; inspect the reported \
                         capability requirements and the host's --accept-capabilities support",
                    );
                } else if host_supports_unsafe
                    && !ctx.allow_unsafe_plugin_install
                    && install_output_looks_like_safety_rejection(&output)
                {
                    reason.push_str(
                        "; this looks like an OpenClaw plugin-safety rejection — review the \
                         reported findings and, only if you accept them, re-run enable with \
                         --allow-unsafe-plugin-install",
                    );
                }
                return Err(AdapterError::FrameworkCli { program, reason });
            }
            Some(plugin_id)
        };

        // The install just ran, and on a host whose plugin declares a slot kind it
        // has already performed the hand-off itself. Record that before the next
        // fallible step rather than in `apply_displacements`, which is four steps
        // later and unreachable if any of them fails — see the method.
        if plugin.is_some() {
            self.record_slot_selection_handoffs(claim, &home, ctx, progress)?;
        }

        for skill in &ctx.declared_skills {
            let src = skill
                .source
                .clone()
                .unwrap_or_else(|| ctx.resource_root.join("skills").join(&skill.name));
            let resource_id = format!("openclaw_skill_{}", skill.name);
            copy_materialized_resource(claim, &resource_id, &src, ctx.ops)?;
        }

        if let Some(plugin_id) = &plugin {
            // Apply only the entries selected during prepare. A selected
            // entry becomes a durable Pending resource before the command,
            // then transitions to Applied after success. Matching resources
            // from re-enable are reused rather than duplicated.
            for i in selected_config_indices {
                let cfg = &ctx.declared_config[i];
                let resource_id = ensure_config_intent(claim, i, cfg)?;
                // Write-ahead persistence closes the mutation-without-receipt
                // window. On timeout/non-zero exit the entry remains Pending,
                // accurately expressing that host state is uncertain.
                progress.persist_claim(claim)?;
                let cmd = build_config_set_cmd(&cfg.key, &cfg.value, &home, user_home);
                let program = cmd.program.clone();
                let output = ctx.ops.run_framework_cli(cmd)?;
                if !output.success() {
                    return Err(AdapterError::FrameworkCli {
                        program,
                        reason: full_failure_reason("config set", &output),
                    });
                }
                confirm_config_applied(claim, &resource_id, cfg)?;
                progress.persist_claim(claim)?;
            }

            // Install preserves an explicit disabled entry left by uninstall.
            let cmd = build_enable_cmd(plugin_id, &home, user_home, enable_accept_capabilities);
            let program = cmd.program.clone();
            let output = ctx.ops.run_framework_cli(cmd)?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: full_failure_reason("plugins enable", &output),
                });
            }

            // `plugins enable` applies the same slot selection the install did, so
            // this is normally a no-op over entries the pass above already recorded;
            // it is here because verification is the next fallible step, and a
            // hand-off the host performed must not depend on it succeeding.
            self.record_slot_selection_handoffs(claim, &home, ctx, progress)?;

            // Post-enable runtime verification: the plugin must report
            // loaded. A non-loaded status surfaces the framework diagnostics
            // and, via the Manager's receipt-first model, leaves a
            // cleanup_failed receipt for later disable.
            self.verify_runtime(plugin_id, &home, user_home, ctx, verify_with_runtime)?;

            // Displace the bundled plugins the receipt claims only after this
            // adapter's own plugin is installed, enabled and verified loaded:
            // freeing the tool names before that point could leave the host
            // with no plugin behind the slot at all.
            self.apply_displacements(
                claim,
                &freshly_claimed_displacements,
                &home,
                user_home,
                ctx,
                progress,
            )?;
        }

        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        // Resolve and validate the receipt's displacement references before
        // anything else, including the read-only probes below. A dangling or
        // mistyped reference means the receipt cannot be trusted to describe the
        // host at all, and every condition that follows — including a `Healthy`
        // summary an operator would act on — would be reporting about a state
        // this driver cannot actually name. Fails closed the same way Qoder's
        // `native_claim` does for an inconsistent receipt.
        let displaced = claim_displaced_plugins(claim)?;
        let mut conditions = Vec::new();

        // 1. Framework detectable?
        let detect = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        conditions.push(AdapterCondition {
            kind: AdapterConditionKind::FrameworkDetected,
            status: bool_status(detect.detected),
            reason: Some(detect.reason.clone()),
            resource: None,
        });

        // 2. Plugin still registered? Skill-only receipts have no plugin
        //    registry entry by design, so status does not require one.
        let plugin_registered = if claim.is_skill_bundle() {
            conditions.push(AdapterCondition {
                kind: AdapterConditionKind::VerificationSupported,
                status: bool_status(detect.detected),
                reason: Some("skill_bundle has no plugin registry entry".to_string()),
                resource: None,
            });
            ConditionStatus::True
        } else {
            let plugin_id = claim_own_plugin(claim)?;
            let (plugin_cond, verify_cond, plugin_registered) = if !detect.detected {
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("framework not detected; cannot verify".to_string()),
                        resource: plugin_id.as_ref().map(|_| ClaimResourceRef {
                            id: RES_PLUGIN.to_string(),
                        }),
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::False,
                        reason: Some("openclaw CLI unavailable".to_string()),
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                )
            } else if let Some(pid) = &plugin_id {
                self.plugin_registered_condition(pid, ctx)
            } else {
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("receipt has no plugin id".to_string()),
                        resource: None,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::True,
                        reason: None,
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                )
            };
            conditions.push(plugin_cond);
            conditions.push(verify_cond);
            plugin_registered
        };

        // A displacement this receipt claims is part of what makes the adapter
        // work, so it is verified next to the adapter's own registration: a
        // bundled plugin somebody re-enabled holds the tool names again and this
        // adapter's same-named tools stop answering, while every other condition
        // here still reads clean. Not probed when the framework is undetectable
        // — `summarize` already reports Degraded for that, and the probe would
        // only add an unactionable Unknown.
        let displaced_condition = if detect.detected && !displaced.is_empty() {
            let condition = self.displaced_plugins_condition(claim, &displaced, ctx);
            conditions.push(condition.clone());
            Some(condition.status)
        } else {
            None
        };

        let summary = summarize(
            claim.status,
            detect.detected,
            plugin_registered,
            displaced_condition,
        );
        Ok(AdapterStatusReport {
            summary,
            conditions,
        })
    }

    fn disable(
        &self,
        claim: &mut AdapterClaim,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<DisableReport, AdapterError> {
        // Disable must clean the state directory recorded at enable time. In
        // particular, pre-fix receipts can point at the legacy resolver's
        // directory while the caller's active OPENCLAW_STATE_DIR points
        // elsewhere.
        let home = claim_state_dir(claim)?;
        // Validate the displacement references *before* the uninstall below, not
        // when the restore runs three steps later. Resolving them is what tells
        // this driver which plugins it may hand back, so a receipt that cannot
        // be resolved must stop the whole disable while nothing has been touched
        // yet: discovering it after `plugins uninstall` would leave the
        // adapter's own plugin removed, the receipt kept, and the bundled plugin
        // still disabled — a partial uninstall driven by a corrupt record.
        let claimed = claim_displaced_plugins(claim)?;
        // Read the unapplied displacements' slots now, while they still describe
        // the hand-off, because the uninstall below rewrites them: OpenClaw resets
        // a memory slot the uninstalled plugin owned back to `memory-core`. The
        // attribution three steps later has to reason from these, not from what its
        // own cleanup left behind — see [`SlotReadings::Captured`].
        let slots = self.capture_slot_readings(&claimed, &home, ctx);
        // Write down whatever that capture recovered, here and not three steps
        // later: the uninstall below destroys the evidence the recovery rests on,
        // and a restore that fails keeps the receipt for a retry which has no
        // capture of its own. It saves what it writes for the same reason, since
        // every save the Manager performs lands after that uninstall — see the
        // method.
        self.record_recovered_slot_selection_handoffs(
            claim, &claimed, &slots, &home, ctx, progress,
        )?;
        // Resolve again, so the restore reasons from the `applied` flags just
        // written rather than from a view of the receipt that predates them. Both
        // would reach the same verdict on an unchanged host, but the enablement
        // half of the attribution is a live read, and deciding twice leaves the two
        // able to disagree: an operator's change inside the window would have the
        // receipt claiming an ownership this same run's report then denies, which
        // is the contradiction the shared predicate exists to rule out.
        let displaced = claim_displaced_plugins(claim)?;
        let mut messages = Vec::new();
        let mut cleanup_complete = true;

        if let Some(plugin_id) = claim_own_plugin(claim)? {
            validate_plugin_id(&plugin_id)?;
            if find_binary_in_path(&openclaw_bin()).is_none() {
                return Ok(DisableReport {
                    cleanup_complete: false,
                    messages: vec![
                        "openclaw CLI not found on PATH; receipt kept so cleanup can be retried"
                            .to_string(),
                    ],
                });
            }

            let cmd = build_uninstall_cmd(&plugin_id, &home, ctx.user_home.as_deref());
            let output = ctx.ops.run_framework_cli(cmd)?;
            if output.success() {
                messages.push(format!("unregistered openclaw plugin '{plugin_id}'"));
            } else if uninstall_reports_missing_plugin(&output, &plugin_id) {
                messages.push(format!(
                    "openclaw plugin '{plugin_id}' was already unregistered"
                ));
            } else if uninstall_reports_untracked_plugin(&output, &plugin_id) {
                // Missing package ownership does not prove that the plugin is
                // gone. Verify against the same instance as the uninstall.
                let mut cmd = build_list_cmd(&home, ctx.user_home.as_deref());
                cmd.args.push("--json".to_string());
                let verification = ctx.ops.run_framework_cli_json(cmd);
                if !verification
                    .as_ref()
                    .is_ok_and(|output| json_list_confirms_plugin_absent(output, &plugin_id))
                {
                    let reason = match verification {
                        Ok(output) => inspect_diagnostics(&output),
                        Err(err) => err.to_string(),
                    };
                    return Ok(DisableReport {
                        cleanup_complete: false,
                        messages: vec![format!(
                            "openclaw plugin '{plugin_id}' has no tracked package install; \
                             `plugins list --json` could not confirm absence: {reason}; \
                             repair the OpenClaw plugin registry and retry disable"
                        )],
                    });
                }
                messages.push(format!(
                    "openclaw plugin '{plugin_id}' is absent from `plugins list --json`"
                ));
            } else {
                return Ok(DisableReport {
                    cleanup_complete: false,
                    messages: vec![format!(
                        "openclaw plugin uninstall failed: {}",
                        cli_failure_reason("plugins uninstall", &output)
                    )],
                });
            }
        } else {
            messages.push("receipt records no plugin to unregister".to_string());
        }

        let skill_resources = claim_skill_resources(claim);
        for skill_name in &skill_resources {
            let skill_dir = home.join("skills").join(skill_name);
            match ctx.ops.remove_tree(&skill_dir) {
                Ok(true) => messages.push(format!(
                    "removed openclaw skill dir {}",
                    skill_dir.display()
                )),
                Ok(false) => {} // already gone, idempotent
                Err(err) => {
                    messages.push(format!(
                        "failed to remove skill dir {}: {err}",
                        skill_dir.display()
                    ));
                    cleanup_complete = false;
                }
            }
        }

        // 3. Hand back every framework plugin this adapter displaced, in the
        //    order claimed. Only what the receipt records is restored: a plugin
        //    the operator had already disabled before enable was never claimed,
        //    so it is never re-enabled here.
        let restore = self.restore_displaced_plugins(
            claim,
            &displaced,
            &home,
            ctx,
            SlotReadings::Captured(&slots),
        )?;
        messages.extend(restore.messages);
        cleanup_complete = cleanup_complete && restore.cleanup_complete;

        // 4. Config entries are NOT reversed on disable (framework-wide
        //    config should persist).
        let (applied_config_count, pending_config_count) = claim_config_counts(claim);
        if applied_config_count > 0 {
            let noun = if applied_config_count == 1 {
                "entry"
            } else {
                "entries"
            };
            messages.push(format!(
                "{applied_config_count} confirmed openclaw config {noun} left in place \
                 (not reversed on disable)"
            ));
        }
        if pending_config_count > 0 {
            let noun = if pending_config_count == 1 {
                "entry"
            } else {
                "entries"
            };
            messages.push(format!(
                "{pending_config_count} openclaw config {noun} with an uncertain apply outcome \
                 left in place (not reversed on disable)"
            ));
        }

        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }
}

impl OpenClawDriver {
    /// Decide what this enable may do about one declared displacement, from the
    /// host's effective plugin inventory, its policy keys, and the plugin's
    /// persisted enablement flag.
    ///
    /// `plugins disable` exits 0 whether or not the plugin was enabled, so its
    /// own status cannot distinguish a real transition from a no-op — the probes
    /// have to. Three of their answers are positive evidence and each means
    /// something different:
    ///
    /// - a readable inventory that does not list the id means the contract names
    ///   a plugin this host does not have, which no amount of retrying fixes;
    /// - an explicit `plugins.deny` entry, or `plugins.enabled = false`, means the
    ///   operator turned it off by policy: the host will neither load it nor
    ///   accept the `plugins enable` a later restore would issue;
    /// - a persisted `enabled = false` means the operator turned it off by hand.
    ///
    /// A restrictive `plugins.allow` that omits the id is *not* in that list,
    /// though it used to be, and both halves of the reason it was are wrong:
    ///
    /// - It does not keep the plugin from loading, so it cannot mean the tool
    ///   names are free. Two of OpenClaw's own load paths never consult it: the
    ///   activation decision enables whichever plugin owns `plugins.slots.memory`
    ///   *before* reaching the allowlist check (v2026.4.14
    ///   `src/plugins/config-state.ts:310-345`, v2026.9.2
    ///   `src/plugins/config-activation-shared.ts:204-218`), and since the
    ///   v2026.7.2 beta line the memory dreaming sidecar is a separate path that
    ///   checks the global switch, the denylist and an explicit `enabled = false`
    ///   and nothing else (`resolveAuthorizedDreamingSidecar`, v2026.9.2
    ///   `src/plugins/loader-shared.ts:90-137`), after which the loader hard-codes
    ///   it to `enabled: true, activated: true` instead of asking
    ///   (`src/plugins/loader-runtime-candidate.ts:108-119,145-146`). With
    ///   `plugins.allow = ["memory-anolisa"]`, `plugins.slots.memory =
    ///   "memory-anolisa"`, dreaming on and `memory-core` not explicitly off, the
    ///   bundled plugin loads and registers `memory_get` / `memory_search`
    ///   (`extensions/memory-core/index.ts:273-279`): the exact collision this
    ///   hand-off exists to release, left standing by a veto that assumed
    ///   otherwise — and with no displacement in the receipt, `status` had nothing
    ///   to check either.
    /// - It does not make the restore impossible, which was the only reason the
    ///   veto was here at all. Whether `plugins enable` honors the allowlist
    ///   changed between versions: v2026.4.14 appends the id to `plugins.allow`
    ///   and reports success (`src/plugins/enable.ts:12-24` with
    ///   `src/config/plugins-allowlist.ts:7-22`), v2026.9.2 refuses it and exits
    ///   non-zero (`src/plugins/enable.ts:38-40` with
    ///   `src/cli/plugins-cli.runtime.ts:218-223`). And it is a question about the
    ///   host *at restore time*, which `restore_decision` asks again then and can
    ///   answer better: this enable's own `plugins install` is about to write the
    ///   slot, and a later `plugins uninstall` removes this plugin from
    ///   `plugins.allow` (v2026.4.14 `src/plugins/uninstall.ts:126-133`), so the
    ///   restriction read here is not the one a restore will meet. It stays a
    ///   veto there — [`AllowlistMeaning::RefusesRestore`] — and stops being one
    ///   here.
    ///
    /// What is left is the asymmetry this function already applies to every answer
    /// the host cannot give. Skipping the claim leaves two plugins fighting over
    /// the tool names, with no receipt recording the ownership and so no
    /// `status` condition that could ever report it; claiming it costs one
    /// `plugins disable` of a plugin the host very likely turned off on its own
    /// anyway, because slot selection writes `enabled = false` for every other
    /// plugin of the selected kind whether this driver asks it to or not.
    ///
    /// A claim is split by how it was reached. `ClaimEnabled` means the host
    /// positively said the plugin was on, so the disable about to happen is this
    /// enable's own transition; `ClaimUnverified` means the host could not answer
    /// and the claim rests on the asymmetry below alone. The first is always
    /// re-confirmed at apply time. The second is re-confirmed only when no prior
    /// receipt for this instance owns it — see the variant's doc for what each
    /// direction costs.
    ///
    /// The last two are the same verdict — not this adapter's transition to undo
    /// — but the first is a broken contract and must fail the enable before
    /// anything is installed. Everything the host *cannot* answer is claimed,
    /// because the two failure modes are not symmetric: skipping the claim there
    /// strands the host with nothing behind the slot after disable, while
    /// claiming it can only cost an operator one extra `plugins disable`.
    fn displacement_probe(
        &self,
        spec: &DisplacedPluginSpec,
        inventory: Option<&str>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> DisplacementProbe {
        // A readable inventory that does not list the id is positive evidence the
        // contract names a plugin this host does not have — a typo, or a bundled
        // plugin this OpenClaw version dropped. Fail before installing anything:
        // the alternative is to install and verify this adapter's own plugin,
        // then discover the mistake when `plugins disable` errors, leaving a
        // receipt whose restore can never converge either.
        if inventory_omits_plugin(inventory, &spec.id) {
            return DisplacementProbe::NotOnHost;
        }
        // No uninstall precedes an enable-side probe, so the key is read as the
        // host holds it. `Allowlist` is not a verdict here either way — see the
        // match arm.
        match self.policy_blocking_plugin(&spec.id, home, ctx, AllowlistReading::Live) {
            PolicyBlock::Denied => {
                return DisplacementProbe::AlreadyOff("excluded by `plugins.deny`".to_string());
            }
            PolicyBlock::PluginsDisabled => {
                return DisplacementProbe::AlreadyOff(
                    "plugins are globally disabled (`plugins.enabled = false`)".to_string(),
                );
            }
            // Not a veto on this side, only at restore time — see the doc above.
            // The enablement probe below decides, and for a plugin the allowlist
            // omits it is the only reading here that the host cannot route around.
            PolicyBlock::Allowlist | PolicyBlock::None => {}
        }
        // Only a positive `Disabled` opts out. `Unknown` is claimed on purpose,
        // per the asymmetry above — but as `ClaimUnverified`, because a claim the
        // host could not confirm is not one the probe can attribute, and whether
        // this enable may later attribute it depends on the prior receipt.
        match self.read_plugin_enablement(&spec.id, home, ctx) {
            PluginEnablement::Disabled => DisplacementProbe::FlagDisabled,
            PluginEnablement::Enabled => DisplacementProbe::ClaimEnabled,
            PluginEnablement::Unknown => DisplacementProbe::ClaimUnverified,
        }
    }

    /// Read `openclaw plugins list` stdout — the host's effective plugin
    /// inventory. `None` when the host cannot answer, which is *not* the same as
    /// an inventory that happens to be empty.
    fn read_plugin_inventory(&self, home: &Path, ctx: &DriverCtx) -> Option<String> {
        let cmd = build_list_cmd(home, ctx.user_home.as_deref());
        let output = ctx.ops.run_framework_cli(cmd).ok()?;
        output.success().then_some(output.stdout)
    }

    /// Read one config key's raw output. `None` when the host cannot answer.
    ///
    /// Kept raw rather than reduced to a token because a policy key holds a
    /// *list*, which `config_answer_token` would collapse to its last line.
    fn read_config_output(&self, key: &str, home: &Path, ctx: &DriverCtx) -> Option<CliOutput> {
        let cmd = build_config_get_cmd(key, home, ctx.user_home.as_deref());
        let output = ctx.ops.run_framework_cli(cmd).ok()?;
        output.success().then_some(output)
    }

    /// The policy key that explicitly keeps `plugin_id` off, when one does.
    ///
    /// `plugins.entries.<id>.enabled` is only one of the ways an operator can
    /// turn a plugin off, and it is not the strongest. Three policy settings
    /// override it:
    ///
    /// - `plugins.enabled = false` — the *global* switch. It refuses every
    ///   `plugins enable`, for every plugin, and says nothing about any one
    ///   plugin's own entry, so reading only the per-plugin keys misses it
    ///   entirely. An operator who flips it after a successful enable would
    ///   otherwise leave this driver retrying a restore that can never converge,
    ///   and the receipt would be kept as a cleanup failure forever instead of
    ///   being recognized as released by policy.
    /// - an explicit `plugins.deny` entry naming the plugin;
    /// - a restrictive `plugins.allow` that omits it.
    ///
    /// The first two mean the same thing to every caller, and both halves of it
    /// hold on every host version this driver runs against: OpenClaw's activation
    /// decision refuses to load the plugin (`resolvePluginActivationState`,
    /// v2026.4.14 `src/plugins/config-state.ts:281-294`), *and* its own load path
    /// that ignores the rest of the policy still refuses it — the memory dreaming
    /// sidecar checks the global switch and the denylist explicitly
    /// (`resolveAuthorizedDreamingSidecar`, v2026.9.2
    /// `src/plugins/loader-shared.ts:101-115`). `enablePluginInConfig` returns
    /// `enabled: false` for exactly these two and nothing else, in both versions
    /// (v2026.4.14 `src/plugins/enable.ts:15-20`, v2026.9.2
    /// `src/plugins/enable.ts:32-37`), so a restore against either really can
    /// never succeed.
    ///
    /// The third does not, and is reported separately so each caller can weigh it
    /// for the question it is asking — see [`AllowlistMeaning`]. It is *not*
    /// evidence the plugin cannot load, because two of the host's load paths
    /// never consult it, and it is not reliably evidence that a restore would be
    /// refused either, because whether `plugins enable` honors the allowlist
    /// changed between versions.
    ///
    /// Only positive evidence counts. An unreadable key, an empty one, or a
    /// host's rendering of "unset" is no verdict, and `plugins.allow` is only
    /// read as restrictive when it names *something*: an allowlist gates
    /// non-bundled installs on many hosts, so treating a vacant answer as
    /// "nothing is allowed" would switch this whole feature off wherever the key
    /// is merely unset. `plugins.enabled` is held to the same rule — only an
    /// explicit `false` counts, because guessing "off" from an unanswerable probe
    /// would skip hand-offs on hosts that are working fine.
    ///
    /// `allowlist` says which reading of `plugins.allow` to weigh — the host's
    /// current one, or the one the operation's own uninstall leaves behind. It
    /// cannot affect the other two keys, which this driver never writes. See
    /// [`AllowlistReading`].
    fn policy_blocking_plugin(
        &self,
        plugin_id: &str,
        home: &Path,
        ctx: &DriverCtx,
        allowlist: AllowlistReading<'_>,
    ) -> PolicyBlock {
        if let Some(output) = self.read_config_output("plugins.enabled", home, ctx)
            && config_answer_is_false(&config_answer_token(&output))
        {
            return PolicyBlock::PluginsDisabled;
        }
        if let Some(output) = self.read_config_output("plugins.deny", home, ctx)
            && let PolicyIdList::Named(ids) = policy_id_list(&output, "plugins.deny")
            && ids.iter().any(|id| id == plugin_id)
        {
            return PolicyBlock::Denied;
        }
        // A restrictive allowlist is a reading, not yet a verdict: only when it
        // actually names something, and what it then means depends on the caller.
        // See the doc above and [`AllowlistMeaning`].
        if let Some(output) = self.read_config_output("plugins.allow", home, ctx)
            && let PolicyIdList::Named(ids) = policy_id_list(&output, "plugins.allow")
        {
            let surviving = allowlist.surviving_allow_ids(ids);
            // "Names something" is asked of the projected list, not of the host's
            // answer, because the projection is what the operation acts on: an
            // allowlist whose only entry the uninstall removes is a key upstream
            // drops, and a dropped key restricts nothing.
            if !surviving.is_empty() && !surviving.iter().any(|id| id == plugin_id) {
                return PolicyBlock::Allowlist;
            }
        }
        PolicyBlock::None
    }

    /// Read the persisted `plugins.entries.<id>.enabled` flag for a framework
    /// plugin.
    ///
    /// `Unknown` covers every answer the host cannot give — no `config get`,
    /// a non-zero exit — and is kept distinct from `Enabled` because the two
    /// callers need different things of it: claiming a displacement deliberately
    /// treats them alike, while `status` must not report a collision it could
    /// not observe. An absent key reads `Enabled`, which is the bundled default.
    fn read_plugin_enablement(
        &self,
        plugin_id: &str,
        home: &Path,
        ctx: &DriverCtx,
    ) -> PluginEnablement {
        let key = format!("plugins.entries.{plugin_id}.enabled");
        let cmd = build_config_get_cmd(&key, home, ctx.user_home.as_deref());
        let Ok(output) = ctx.ops.run_framework_cli(cmd) else {
            return PluginEnablement::Unknown;
        };
        if !output.success() {
            return PluginEnablement::Unknown;
        }
        let token = config_answer_token(&output);
        if token.is_empty() {
            return PluginEnablement::Enabled;
        }
        if config_answer_is_false(&token) {
            PluginEnablement::Disabled
        } else {
            PluginEnablement::Enabled
        }
    }

    /// Verify that every framework plugin this receipt claims as displaced is
    /// still displaced.
    ///
    /// `apply_enable` releases the tool names exactly once, and two different
    /// hosts can look identical in config afterwards. On one, a later
    /// `openclaw plugins enable <displaced>` — an operator command, a framework
    /// update — put the first-wins collision straight back, and this adapter's
    /// own plugin still lists and loads fine while its colliding tools silently
    /// stop answering. On the other, nothing re-enabled it but the operator has
    /// not restarted the gateway yet, so the *running* plugin still holds the
    /// names even though the config already says disabled. Only the first is
    /// decidable from config; the second is reported as unverified rather than
    /// guessed at, see below.
    ///
    /// The persisted enablement flag settles one direction on its own: a plugin
    /// that reads *enabled* is not displaced, whether or not the gateway has
    /// caught up, so that reports `False`. The other direction it cannot settle.
    /// "Disabled in config" proves the hand-off was recorded and nothing about
    /// whether the running gateway applied it, and this driver has no channel to
    /// that gateway — `plugins inspect --runtime` spawns a fresh CLI process that
    /// reads the same config, so it echoes the config back rather than reporting
    /// what is loaded. That reports `Unknown`, and the reason says so without
    /// promising that anything the operator can do will turn it into a verdict:
    /// restarting the gateway changes what OpenClaw serves but not what ANOLISA
    /// can observe, so a message implying "restart and re-check" would send the
    /// operator around a loop with no exit. It does not assert that a restart is
    /// what makes the hand-off take effect either — whether one is needed at all
    /// depends on the host's plugin reload mode, which this driver cannot read,
    /// and telling a hot-reloading host to restart would buy an unnecessary
    /// gateway interruption. It points at an actual tool call as the way
    /// to confirm it — the only check that travels through the running gateway.
    ///
    /// That advice is deliberately generic. `displaces` is not an agent-memory
    /// contract; any adapter may declare a displacement for its own colliding
    /// tools, so a reason that hardcoded `memory_get` would send every other
    /// adapter's operator after a tool it does not register and could not use to
    /// confirm anything. Naming the tool belongs in that component's own
    /// documentation. It does *not* point at `plugins list` or `plugins inspect`: both
    /// read the persisted registry and config, so after enable and before a
    /// restart they show the bundled plugin disabled while the old gateway may
    /// still be serving its tools. That is a false confirmation, and it is worse
    /// than offering no check at all, because it moves the operator from "unknown"
    /// to "believes it is fixed". Reaching a decisive verdict here needs a gateway
    /// tool-catalog channel; until one exists this stays `Unknown` rather than
    /// becoming the false all-clear it exists to prevent.
    ///
    /// Every entry is probed against the host, including one whose hand-off never
    /// ran. [`DisplacedPluginRef::applied`] is provenance — who turned the plugin
    /// off — and is deliberately not allowed to stand in for host state here: an
    /// entry this adapter never disabled is *more* likely to still be on, not less,
    /// since taking the names off it was the point of the enable that recorded it.
    /// So an unapplied entry that the host reports enabled counts as a collision
    /// (`False`), and one it reports off counts as `Unknown` either way — with the
    /// provenance stated in the reason, because "off, and not by this adapter" and
    /// "off, by this adapter's own slot selection, with the record lost" lead to
    /// different `disable` behavior and must not read the same. Nothing the persisted state can say counts as
    /// a *release*: a plugin the host no longer lists, and one a policy keeps from
    /// loading, are both evidence about what a freshly spawned CLI sees and what the
    /// config asks for, and neither establishes that the gateway already running
    /// stopped serving those tools. Both are `Unknown`, and both say so in the reason
    /// rather than leaving the operator to guess which of the two they are looking at.
    ///
    /// `displaced` is the caller's already-validated resolution of the receipt's
    /// references and must be non-empty; `status` skips the condition entirely
    /// for a receipt that claims no displacement, so adapters without one keep
    /// their existing condition set.
    fn displaced_plugins_condition(
        &self,
        claim: &AdapterClaim,
        displaced: &[DisplacedPlugin],
        ctx: &DriverCtx,
    ) -> AdapterCondition {
        let kind = AdapterConditionKind::DisplacedPluginsReleased;
        let unresolved = |reason: String| AdapterCondition {
            kind,
            status: ConditionStatus::Unknown,
            reason: Some(reason),
            resource: None,
        };
        // Probe the instance the *receipt* names, not the one the caller's
        // environment happens to point at. A receipt records the state directory
        // it took ownership in, and `OPENCLAW_HOME` / `OPENCLAW_STATE_DIR` can
        // have moved since — `disable` has resolved it this way all along (see
        // its own comment), and a status that probed somewhere else would report
        // on a host this receipt never touched. That is not hypothetical: with
        // `OPENCLAW_HOME=A` still set and `OPENCLAW_STATE_DIR=B` overriding it, a
        // B that happens to have this adapter registered and `memory-core`
        // disabled reads clean, and the collision already restored in A goes
        // unreported.
        let home = match claim_state_dir(claim) {
            Ok(home) => home,
            Err(err) => return unresolved(err.to_string()),
        };
        let home = home.as_path();
        // Read the inventory once for every entry, the way the restore branch
        // does: it is one `plugins list` call, and an unreadable answer is not the
        // same as an empty one.
        let inventory = self.read_plugin_inventory(home, ctx);
        // Read once for every entry: telling a lost hand-off record from a plugin
        // this adapter never turned off needs to know which plugin this adapter's own
        // slot selection would have named.
        let own_plugin_id = match claim_own_plugin(claim) {
            Ok(id) => id,
            Err(err) => return unresolved(err.to_string()),
        };
        let mut re_enabled = Vec::new();
        let mut recorded_only = Vec::new();
        let mut unreadable = Vec::new();
        // Gone from the host's inventory. Not a collision, and not a release
        // either: see the branch.
        let mut absent: Vec<String> = Vec::new();
        // Kept from loading by a persisted policy. Not a collision either — but not
        // a release, because a config write says nothing about the process already
        // running. See the branch for the two directions this can be wrong in.
        let mut policy_blocked: Vec<(String, String)> = Vec::new();
        // Recorded but never performed, and the host says the plugin is on: the
        // names are being held against this adapter right now.
        let mut never_displaced = Vec::new();
        // Recorded but never performed, and the plugin is off for reasons this
        // receipt does not own.
        let mut never_displaced_off = Vec::new();
        // Recorded but never *marked* as performed, while the host shows this
        // adapter's own slot selection having done it: a hand-off in force whose
        // record was lost. Same verdict as `recorded_only`, different provenance —
        // and the difference is not cosmetic, because `disable` hands this one back.
        let mut unrecorded_but_ours = Vec::new();
        for entry in displaced {
            // Ask whether the plugin can hold the names *at all* before asking
            // whether it is switched on — the same two questions, in the same
            // order, that `restore_decision` asks. Reading only the per-plugin
            // flag reported a collision for a plugin a framework upgrade had
            // removed, or one an effective policy keeps from loading, and degraded
            // the whole adapter over a hand-off nothing was contesting.
            // `DoesNotKeepItOff`: an allowlist that omits the plugin says nothing
            // about whether the running gateway is serving its tools, so this
            // falls through to the enablement flag below like any other unread
            // policy. See [`OpenClawDriver::displacement_probe`].
            match self.displacement_block(
                &entry.plugin_id,
                inventory.as_deref(),
                home,
                ctx,
                AllowlistMeaning::DoesNotKeepItOff,
                // `status` describes the host as it stands, and nothing it reads
                // is about to be uninstalled by the call itself.
                AllowlistReading::Live,
            ) {
                // Not a collision, and — since the same review round that fixed the
                // policy branch — not a release either. `plugins list` is answered by
                // a freshly spawned CLI reading the host's current discovery set, so
                // "absent" says the plugin is gone from *that*, not that the gateway
                // already running stopped serving its tools: a module loaded before
                // the removal stays loaded, and OpenClaw's own `plugins uninstall`
                // asks for a gateway restart precisely because removal is not
                // applied to the running process. The argument that used to justify
                // releasing here — that nothing can make it shadow this adapter
                // again without a reinstall, which puts it back in the inventory —
                // is about *future* loading, and the question this condition asks is
                // about the process serving tools *now*. Same missing evidence as
                // the policy branch, different trigger.
                DisplacementBlock::Absent => {
                    absent.push(entry.plugin_id.clone());
                    continue;
                }
                // Not `released`: with `gateway.reload.mode = off` OpenClaw's config
                // reloader explicitly leaves the running process alone, so a deny
                // added after the hand-off does not establish that the gateway
                // stopped serving those tools. Reporting the displacement as
                // released here rolled a recorded hand-off from `Unknown` up to
                // `Healthy` on the strength of a config write — the false all-clear
                // this condition exists to prevent, arriving exactly when the
                // operator may still need to restart the gateway or check a real
                // tool call. Not `re_enabled` either, and that is the half the
                // inverse mistake got wrong: a plugin this policy keeps from loading
                // is not contesting anything the operator can act on, so degrading
                // the whole adapter over it would send them after a collision the
                // host is actively preventing.
                DisplacementBlock::Denied => {
                    policy_blocked.push((
                        entry.plugin_id.clone(),
                        "`plugins.deny` names it, so OpenClaw loads it on no path".to_string(),
                    ));
                    continue;
                }
                DisplacementBlock::PluginsGloballyDisabled => {
                    policy_blocked.push((
                        entry.plugin_id.clone(),
                        "`plugins.enabled` is false, so OpenClaw loads no plugin at all"
                            .to_string(),
                    ));
                    continue;
                }
                // `AllowlistExcluded` is unreachable by construction here: this
                // caller passes [`AllowlistMeaning::DoesNotKeepItOff`], which is
                // exactly the statement that an allowlist does not rule the plugin
                // out. Folded into `None` rather than an `unreachable!()` so a later
                // caller that does ask the other question degrades to the honest
                // verdict — the enablement flag below — instead of panicking in
                // somebody's `status`.
                DisplacementBlock::AllowlistExcluded | DisplacementBlock::None => {}
            }
            // Probed for *every* entry, applied or not. `applied` is a statement
            // about provenance — who turned the plugin off — and says nothing about
            // whether it is currently on, so it cannot substitute for asking.
            match (
                entry.applied,
                self.read_plugin_enablement(&entry.plugin_id, home, ctx),
            ) {
                // Config says on. That verdict needs no gateway: whether the
                // running host has picked it up yet or only will on the next
                // restart, the hand-off this adapter owns is not in force.
                (true, PluginEnablement::Enabled) => re_enabled.push(entry.plugin_id.clone()),
                (true, PluginEnablement::Unknown) | (false, PluginEnablement::Unknown) => {
                    unreadable.push(entry.plugin_id.clone())
                }
                // Config says off — which proves the hand-off was *recorded*, and
                // nothing more. `plugins disable` only writes config; when the
                // running gateway picks that up depends on the host's plugin
                // reload mode, so until then the running plugin may still hold
                // the tool names. Telling those two
                // hosts apart needs a channel to the live gateway, and this
                // driver has none: `plugins inspect --runtime` spawns a new CLI
                // process that reads the same config and inspects runtime in that
                // process, so it reports the config back and cannot see what the
                // gateway actually loaded. Reporting Healthy on its word would
                // be exactly the false all-clear this condition exists to
                // prevent, so the honest verdict is Unknown until a real gateway
                // probe exists.
                (true, PluginEnablement::Disabled) => recorded_only.push(entry.plugin_id.clone()),
                // The hand-off never ran, so this adapter is not what would have
                // taken the names off it — and the host says it is on. That is the
                // collision this condition exists to report, not a release.
                (false, PluginEnablement::Enabled) => never_displaced.push(entry.plugin_id.clone()),
                // Off, and the receipt never marked the hand-off. Two stories, and
                // the host tells them apart: when this adapter's own selection is
                // what turned it off, this is a hand-off in force whose record was
                // lost, and filing it under "not by this adapter" would contradict
                // the restore `disable` is about to perform. The enablement flag was
                // just read, so only the slot half of the predicate is needed here.
                (false, PluginEnablement::Disabled) => {
                    if matches!(
                        self.slot_attribution(
                            entry,
                            own_plugin_id.as_deref(),
                            SlotReadings::Live,
                            home,
                            ctx
                        ),
                        SlotAttribution::Own
                    ) {
                        unrecorded_but_ours.push(entry.plugin_id.clone())
                    } else {
                        never_displaced_off.push(entry.plugin_id.clone())
                    }
                }
            }
        }
        // Every bucket is reported, not just the highest-priority one. A receipt
        // may displace several plugins and each can be in a different state, so an
        // if/else-if chain that formatted only the winning bucket silently dropped
        // the others: an operator would fix the one plugin named, re-run status,
        // and only then discover the next. The *status* still takes the worst
        // verdict — a plugin positively back on outweighs one that could not be
        // checked — but the reason carries all of them.
        // `absent` and `policy_blocked` both appear, in the `Unknown` group: neither
        // is a collision the operator can act on, and neither is an observation of
        // the running gateway — one is a cold inventory read, the other a persisted
        // instruction — and this driver has no channel to one. Every bucket is
        // therefore either a collision or unobservable, which is the honest shape of
        // a verdict with no gateway to ask; the `True` arm below is what a future
        // bucket with positive evidence would reach, and keeping it explicit means
        // adding one cannot silently change today's verdicts.
        //
        // An *unapplied* entry is the opposite, and it used to be filed under the
        // same argument — which was exactly backwards, and contradicted this
        // function's own reason text for that bucket ("this adapter never disabled
        // it"). Not having performed the hand-off says nothing about whether the
        // plugin is on; it makes it *more* likely, since taking the names off it
        // was the entire point of the enable that recorded the entry. So an
        // unapplied entry is probed like any other and counts toward `False` when
        // the host says it is enabled. Filing it under `True` instead let a crash
        // between the Manager's write-ahead persist and `apply_displacements`
        // report `Healthy` — receipt `Enabled`, own plugin registered and verified
        // loaded, displacement "released" — with the bundled plugin still serving
        // every tool name this adapter registers, which is the precise false
        // all-clear this condition exists to prevent.
        let status = if !re_enabled.is_empty() || !never_displaced.is_empty() {
            ConditionStatus::False
        } else if !recorded_only.is_empty()
            || !unreadable.is_empty()
            || !never_displaced_off.is_empty()
            || !unrecorded_but_ours.is_empty()
            || !policy_blocked.is_empty()
            || !absent.is_empty()
        {
            ConditionStatus::Unknown
        } else {
            ConditionStatus::True
        };

        // One command and one config key per plugin, never a joined list: a
        // receipt may displace several, and `openclaw plugins disable a, b` passes
        // one malformed argument while `plugins.entries.a, b.enabled` is a key that
        // has never existed. A verdict the operator cannot act on by copying it is
        // worse than one that only names the problem.
        let mut parts: Vec<String> = Vec::new();
        if !re_enabled.is_empty() {
            let names = re_enabled.join(", ");
            let commands = re_enabled
                .iter()
                .map(|id| format!("`openclaw plugins disable {id}`"))
                .collect::<Vec<_>>()
                .join(" and ");
            parts.push(format!(
                "openclaw plugin '{names}' was re-enabled after this adapter displaced it, so it \
                 holds the tool names again and this adapter's own same-named tools are dropped; \
                 run `anolisa adapter disable {component}` and enable again, or re-disable each \
                 one yourself with {commands}.",
                component = claim.component
            ));
        }
        if !recorded_only.is_empty() {
            let names = recorded_only.join(", ");
            // Deliberately conditional about the restart. Whether a config write
            // needs one depends on the host's plugin reload mode — a mode that
            // hot-reloads `plugins.entries.*` applies the change by itself — and
            // this driver cannot read that mode, so asserting "restart is what
            // applies it" would tell every host that does not need one to take an
            // unnecessary gateway interruption.
            parts.push(format!(
                "openclaw plugin '{names}' is disabled in config, so the hand-off is recorded, \
                 but ANOLISA cannot observe the running gateway and so cannot confirm it has \
                 taken effect. Whether anything further is needed depends on this host's plugin \
                 reload mode: one that hot-reloads `plugins.entries.*` applies the change by \
                 itself, and one that does not needs `openclaw gateway restart`. Either way a \
                 restart does not change this verdict — ANOLISA still has no channel to the \
                 gateway — so read `unknown` here as unobservable, not as something a restart \
                 will settle. To check whether it took, call one of this adapter's own tools \
                 that '{names}' also provides and see which plugin answers: only a real tool \
                 call travels through the running gateway. This component's own documentation \
                 names that tool."
            ));
        }
        if !unreadable.is_empty() {
            let names = unreadable.join(", ");
            let keys = unreadable
                .iter()
                .map(|id| format!("plugins.entries.{id}.enabled"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "Cannot read {keys}, so whether '{names}' holds the tool names is unverified."
            ));
        }
        if !absent.is_empty() {
            let names = absent.join(", ");
            parts.push(format!(
                "openclaw plugin '{names}' is no longer in this host's `plugins list` inventory, \
                 so there is nothing to re-disable and no way for it to start holding the tool \
                 names again without being reinstalled — but that inventory is a cold read from \
                 a freshly spawned CLI, and a gateway that loaded the plugin before it was \
                 removed keeps serving its tools until it is restarted, which is why OpenClaw's \
                 own `plugins uninstall` asks for one. Whether the running gateway has stopped is \
                 not observable from here, so this stays unknown rather than cleared; the only \
                 check that settles it is a real tool call through the gateway, which this \
                 component's own documentation names."
            ));
        }
        if !policy_blocked.is_empty() {
            let detail = policy_blocked
                .iter()
                .map(|(id, why)| format!("'{id}' ({why})"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "openclaw plugin {detail}, so it is not contesting the tool names and there is \
                 nothing to re-disable — but that is a persisted config write, not an \
                 observation of the running gateway, and ANOLISA has no channel to one: with \
                 `gateway.reload.mode = off` OpenClaw's config reloader leaves the running \
                 process untouched, so a policy added after the hand-off does not establish \
                 that the gateway stopped serving those tools. This stays unknown rather than \
                 healthy for the same reason a recorded hand-off does; the only check that \
                 settles it is a real tool call through the gateway, which this component's \
                 own documentation names."
            ));
        }
        if !never_displaced.is_empty() {
            let names = never_displaced.join(", ");
            let commands = never_displaced
                .iter()
                .map(|id| format!("`openclaw plugins disable {id}`"))
                .collect::<Vec<_>>()
                .join(" and ");
            parts.push(format!(
                "openclaw plugin '{names}' is enabled on this host and this adapter never \
                 disabled it: the enable that recorded this displacement did not reach the \
                 hand-off, so the tool names were never released and this adapter's own \
                 same-named tools are dropped. Run `anolisa adapter disable {component}` and \
                 enable again to perform the hand-off, or disable each one yourself with \
                 {commands}.",
                component = claim.component
            ));
        }
        if !unrecorded_but_ours.is_empty() {
            let names = unrecorded_but_ours.join(", ");
            parts.push(format!(
                "openclaw plugin '{names}' is disabled in config and this receipt never marked \
                 the hand-off as issued, but the slot it names reads as this adapter's own \
                 plugin — the state OpenClaw's exclusive slot selection leaves behind when \
                 installing that plugin turned this one off — so this adapter is what turned it \
                 off, and `anolisa adapter disable` will hand it back. As far as config shows the \
                 hand-off is in force; whether the running gateway applied it is not observable \
                 from here, so this stays unknown rather than healthy, and the only check that \
                 settles it is a real tool call through the gateway, which this component's own \
                 documentation names."
            ));
        }
        if !never_displaced_off.is_empty() {
            let names = never_displaced_off.join(", ");
            parts.push(format!(
                "openclaw plugin '{names}' is disabled in config, but not by this adapter: the \
                 enable that recorded this displacement never reached the hand-off, so this \
                 receipt is not what turned it off. The tool names look free as far as config \
                 shows and ANOLISA cannot observe the running gateway, so this stays unknown \
                 rather than healthy; re-run the enable to make the hand-off this adapter's own."
            ));
        }
        let reason = (!parts.is_empty()).then(|| parts.join(" "));

        // A single displaced plugin is the common case, and pointing the condition
        // at its receipt resource lets a machine consumer locate it rather than
        // parse the prose. With several there is no one resource to name, so the
        // reason's per-plugin naming is what carries it.
        let resource = match displaced {
            [only] => {
                // The id the receipt actually names, not one re-derived from the
                // plugin id: re-deriving produced a reference the receipt did not
                // contain, so a machine consumer got `None` for the one case the
                // reference exists to serve. `claim_displaced_plugins` has already
                // checked the resource resolves, so this is that same id.
                let id = only.resource.clone();
                claim
                    .resource(&id)
                    .is_some()
                    .then_some(ClaimResourceRef { id })
            }
            _ => None,
        };
        AdapterCondition {
            kind,
            status,
            reason,
            resource,
        }
    }

    /// Record the hand-offs this enable's *own* commands already performed, before
    /// anything fallible runs after them.
    ///
    /// `plugins install` and `plugins enable` both apply OpenClaw's exclusive slot
    /// selection, which for a same-kind displacement turns the bundled plugin off —
    /// see [`Self::slot_attribution`]. `apply_displacements` recognizes that state and
    /// keeps the claim, but it runs only after `verify_runtime`, and it is
    /// `mark_displacement_applied` that turns a claim into a *restoration* record. So
    /// a verification failure — an ordinary failed enable, no operator and no
    /// concurrency involved — left a receipt still reading `applied = false` over a
    /// host where the bundled plugin was off because of this enable. `restore_decision`
    /// answers `SkipNotApplied` for that, so disable told the operator "this adapter
    /// never disabled it", reported a complete cleanup, removed the receipt, and left
    /// the host with no memory backend and nothing recording why.
    ///
    /// That is the same mistake the write-ahead marks elsewhere in this driver exist
    /// to prevent, arriving through a transition the host performs on this driver's
    /// behalf instead of one it issues itself. So the record is made as soon as the
    /// evidence for it exists. Only positive evidence marks anything: the slot naming
    /// this adapter's own plugin *and* the plugin's own flag reading off. An entry
    /// this enable did not turn off stays unapplied, and a later disable still
    /// restores nothing it should not.
    fn record_slot_selection_handoffs(
        &self,
        claim: &mut AdapterClaim,
        home: &Path,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<(), AdapterError> {
        let claimed = claim_displaced_plugins(claim)?;
        if claimed.is_empty() {
            return Ok(());
        }
        let own_plugin_id = claim_own_plugin(claim)?;
        for entry in &claimed {
            // Already recorded, so there is nothing to attribute and nothing to
            // probe: this runs twice per enable, and the second pass is free.
            if entry.applied {
                continue;
            }
            validate_plugin_id(&entry.plugin_id)?;
            // Positive evidence only — see the predicate. An entry this cannot
            // attribute stays unapplied, which is what keeps a later disable from
            // restoring a plugin this adapter never turned off.
            if !self.handoff_performed_by_slot_selection(
                entry,
                own_plugin_id.as_deref(),
                SlotReadings::Live,
                home,
                ctx,
            ) {
                continue;
            }
            mark_displacement_applied(claim, &entry.resource)?;
            progress.persist_claim(claim)?;
        }
        Ok(())
    }

    /// Disable every framework plugin the receipt claims as displaced, having
    /// re-confirmed each claim immediately beforehand.
    ///
    /// A failure *after* the command ran still leaves the receipt claiming the
    /// transition, and that is the safe direction: `plugins disable` is
    /// idempotent so a retry converges, whereas losing the claim would leave the
    /// bundled plugin off with nothing recording why.
    ///
    /// The converse is not safe, and is what the re-confirmation is for. An
    /// earlier version of this comment argued that re-enabling a plugin this
    /// adapter never actually disabled is "a no-op". Within the window between
    /// `prepare_enable` and here, it is not: install, config writes and runtime
    /// verification all run in between, and the Manager's lock serializes ANOLISA
    /// against itself — not against an operator typing `openclaw plugins disable
    /// memory-core`, nor against a framework update. If the plugin is already off
    /// by the time this runs, `plugins disable` exits 0 having changed nothing,
    /// the receipt still says ANOLISA owns that transition, and a later
    /// `adapter disable` re-enables a plugin the operator had just closed. That is
    /// undoing somebody else's action, not a no-op — so the claim is released
    /// instead, which is the only outcome that leaves the receipt telling the
    /// truth.
    ///
    /// "Somebody else" has to be *established*, though, because this driver's own
    /// commands are among the candidates. `plugins install` and `plugins enable` —
    /// both of which have already run by the time this is reached — apply OpenClaw's
    /// exclusive slot selection, which turns every other plugin of the same kind off
    /// and records the slot as naming the plugin they selected. So a re-probe reading
    /// `enabled = false` here is ambiguous by construction, and releasing on it
    /// deleted the only record that the bundled plugin was off: the receipt claimed
    /// no displacement, disable reported a complete cleanup and removed it, and the
    /// host was left with no memory backend and nothing that would restore one. A
    /// bare flag is therefore attributed through the slot that same selection wrote —
    /// see [`Self::slot_selection_explains_off`] — and only an explanation this
    /// driver cannot have caused releases a claim on its own.
    fn apply_displacements(
        &self,
        claim: &mut AdapterClaim,
        freshly_claimed: &[String],
        home: &Path,
        user_home: Option<&Path>,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<(), AdapterError> {
        let claimed = claim_displaced_plugins(claim)?;
        if claimed.is_empty() {
            return Ok(());
        }
        let inventory = self.read_plugin_inventory(home, ctx);
        // Read once for every entry: attributing a `false` flag needs to know which
        // plugin this adapter's own slot selection would have named.
        let own_plugin_id = claim_own_plugin(claim)?;
        for entry in &claimed {
            let plugin_id = entry.plugin_id.clone();
            validate_plugin_id(&plugin_id)?;
            // Re-confirm at the last read-only moment before the mutation — but
            // only for a claim this enable can attribute to itself. Absent from
            // `freshly_claimed` is a claim carried over from a prior receipt, and
            // one prepare could not verify *that the same prior receipt already
            // owns*; in both cases the plugin is off because of an earlier enable
            // of this adapter, so a re-probe reading `false` cannot be attributed
            // to anybody else and releasing on it would delete the only remaining
            // record of why the plugin is off. An unverified claim with no prior
            // ownership behind it *is* re-confirmed — see
            // [`inherited_displacement_ids`].
            //
            // This narrows the window; it cannot close it, because OpenClaw offers
            // no conditional write — between this probe and the command below the
            // host can still change. Closing it properly needs a framework-level
            // lock or a compare-and-swap the CLI does not expose.
            let spec = DisplacedPluginSpec {
                id: plugin_id.clone(),
                slot: entry.slot.clone(),
            };
            // Release only on positive evidence that the plugin is off or gone for
            // a reason this enable did not cause. An *unanswerable* re-probe is not
            // such evidence — prepare did see it enabled, and "could not read" now
            // says nothing about who turned it off, so keeping the claim is the
            // honest reading.
            if freshly_claimed.contains(&plugin_id) {
                let released_by_somebody_else =
                    match self.displacement_probe(&spec, inventory.as_deref(), home, ctx) {
                        // The host does not have the plugin, or a policy key keeps it
                        // off: neither is a state any command this driver runs
                        // writes, so both stay attributable to a third party — and a
                        // policy would refuse the restore anyway.
                        DisplacementProbe::NotOnHost | DisplacementProbe::AlreadyOff(_) => true,
                        // The flag alone cannot say whose doing it is, because this
                        // enable's own install and enable both write it for a
                        // same-kind plugin. Two answers rule this driver out: a
                        // *readable* slot naming somebody else, and a declaration
                        // that named no slot at all — selection only disables
                        // plugins competing for the selected kind's slot, so a
                        // plugin the contract says holds no slot is not one this
                        // enable could have turned off. One naming this adapter's
                        // own plugin is this driver's hand-off already performed,
                        // and one the host cannot answer is evidence of nothing at
                        // all. Those two keep the claim.
                        DisplacementProbe::FlagDisabled => {
                            matches!(
                                self.slot_attribution(
                                    entry,
                                    own_plugin_id.as_deref(),
                                    SlotReadings::Live,
                                    home,
                                    ctx
                                ),
                                SlotAttribution::Other | SlotAttribution::NotDeclared
                            )
                        }
                        DisplacementProbe::ClaimEnabled | DisplacementProbe::ClaimUnverified => {
                            false
                        }
                    };
                if released_by_somebody_else {
                    release_displacement_claim(claim, &entry.resource)?;
                    // Persist the release before moving on, so a crash here cannot
                    // leave a receipt claiming a transition this adapter did not make.
                    progress.persist_claim(claim)?;
                    continue;
                }
            }
            // Mark the hand-off as issued, then persist — both *before* the
            // command, mirroring the config journal's write-ahead rule. The
            // ordering is what keeps the two crash windows split the way this
            // driver splits everything else: a crash before the command leaves
            // the entry unapplied, so a later disable restores nothing it should
            // not, while a crash after it leaves the entry applied, so the host
            // is not stranded with a plugin nobody owns. Marking afterwards would
            // trade the first for the second, and a stranded host costs more than
            // one unwanted `plugins enable`.
            mark_displacement_applied(claim, &entry.resource)?;
            // Write-ahead persistence, mirroring the config journal: close the
            // mutation-without-receipt window before the command runs.
            progress.persist_claim(claim)?;
            let cmd = build_disable_cmd(&plugin_id, home, user_home);
            let program = cmd.program.clone();
            let output = ctx.ops.run_framework_cli(cmd)?;
            if !output.success() {
                return Err(AdapterError::FrameworkCli {
                    program,
                    reason: format!(
                        "{}; while '{plugin_id}' stays loaded it keeps the tool names this \
                         adapter registers, and the framework's first-wins tool registry drops \
                         this adapter's own",
                        full_failure_reason("plugins disable", &output)
                    ),
                });
            }
        }
        Ok(())
    }

    /// What rules a displaced plugin out, asked of the host rather than of the
    /// receipt: it has left the inventory, or an effective policy keeps it from
    /// loading at all.
    ///
    /// Shared by [`Self::restore_decision`] and [`Self::displaced_plugins_condition`]
    /// because they used to answer it differently — the restore branch consulted
    /// the inventory and both policy levels, while `status` consulted neither and
    /// read only `plugins.entries.<id>.enabled`. A plugin a framework upgrade
    /// removed from the host, or one `plugins.enabled = false` / `plugins.deny`
    /// keeps off, cannot load on any path the host has and so cannot shadow
    /// anything; `status` reported it as a collision anyway and degraded the whole
    /// adapter over a hand-off nothing was contesting.
    ///
    /// The two do *not* ask the same question about a restrictive `plugins.allow`,
    /// and that is what `allowlist` is for. It settles whether the host would
    /// refuse the `plugins enable` a restore issues; it says nothing about whether
    /// the plugin is loaded, because two of the host's own load paths never consult
    /// it — see [`AllowlistMeaning`] and [`Self::displacement_probe`].
    ///
    /// What the two callers *do* with the answer is deliberately not the same, and
    /// the difference is the question each is answering. The restore branch asks
    /// "can this be handed back at all", and for a blocked plugin the answer is no —
    /// the host would refuse the `plugins enable`, so retrying could never succeed
    /// and the ownership is given up. `status` asks "is the running gateway still
    /// serving those tools", and neither a cold inventory read nor a persisted
    /// policy answers that: both leave the verdict `Unknown`, which is not a
    /// collision and not an all-clear. Reading the same probe and reaching different
    /// verdicts is the point; reaching different *facts* would be the drift this
    /// sharing exists to prevent.
    ///
    /// An *unreadable* inventory is [`DisplacementBlock::None`], not `Absent`:
    /// only a readable list that omits the id is evidence the host does not have
    /// it, which is the same rule the enable-side probe applies.
    ///
    /// `allowlist` is the one place the two callers genuinely differ, and it is a
    /// parameter rather than two functions because everything else about the
    /// question is shared and drifting apart is what this helper exists to
    /// prevent. A restrictive `plugins.allow` that omits the plugin blocks a
    /// restore and does not block loading — see [`AllowlistMeaning`].
    ///
    /// `allowlist_reading` is the other half of the same key and is a parameter
    /// for the same reason: it says whether the reading is the host's current one
    /// or the one the operation's own uninstall leaves behind, and getting it
    /// wrong makes a preview contradict the operation — see [`AllowlistReading`].
    fn displacement_block(
        &self,
        plugin_id: &str,
        inventory: Option<&str>,
        home: &Path,
        ctx: &DriverCtx,
        allowlist: AllowlistMeaning,
        allowlist_reading: AllowlistReading<'_>,
    ) -> DisplacementBlock {
        if inventory_omits_plugin(inventory, plugin_id) {
            return DisplacementBlock::Absent;
        }
        match self.policy_blocking_plugin(plugin_id, home, ctx, allowlist_reading) {
            PolicyBlock::Denied => DisplacementBlock::Denied,
            PolicyBlock::PluginsDisabled => DisplacementBlock::PluginsGloballyDisabled,
            PolicyBlock::Allowlist => match allowlist {
                AllowlistMeaning::RefusesRestore => DisplacementBlock::AllowlistExcluded,
                AllowlistMeaning::DoesNotKeepItOff => DisplacementBlock::None,
            },
            PolicyBlock::None => DisplacementBlock::None,
        }
    }

    /// Decide what a restore of one displaced plugin will do, from the receipt and
    /// the host as it reads right now.
    ///
    /// This is the single source of truth for the restore branch, and the
    /// *disable-side* previews call it directly — through `plan_disable_restores`,
    /// `plan_dropped_displacement_restores` and the migration plan. An
    /// `plan_enable` preview cannot: it describes a disable that has not happened
    /// yet, so it enumerates this branch set through [`restore_conditions_note`]
    /// rather than predicting it. Both sides used to compose their own wording,
    /// which is how a preview came to describe two of the four vetoes: it said a slotless
    /// restore was unconditional when the inventory and policy vetoes do not look
    /// at the slot at all, and it promised a `plugins enable` that the real disable
    /// then declined to issue. Because every veto leaves `cleanup_complete` alone
    /// and the receipt is removed either way, that divergence left no trace an
    /// operator could find afterwards — no receipt, no claim, and no displacement
    /// condition for `status` to report.
    ///
    /// The order is the order the real restore applies: the receipt's own record
    /// of whether the hand-off ran, then inventory, then policy (global switch,
    /// denylist, allowlist), then the slot. Only the first reads the receipt; the
    /// rest ask the host.
    ///
    /// `slots` is where the two slot readings come from, and a real `disable`
    /// passes the ones it captured before its own `plugins uninstall` rewrote
    /// them. See [`SlotReadings`].
    ///
    /// `allowlist_reading` settles the same ordering question for the one policy
    /// key that uninstall rewrites, and the two callers stand on opposite sides of
    /// it. The real restore runs *after* the uninstall, so its live read already is
    /// the post-uninstall answer. A preview runs *before* the operation it
    /// describes, so its live read still names this adapter's own plugin and has to
    /// be projected — otherwise the plan and the operation contradict each other on
    /// exactly the allowlist this driver's own enable leaves behind. See
    /// [`AllowlistReading`].
    fn restore_decision(
        &self,
        entry: &DisplacedPlugin,
        host: &RestoreHost<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> RestoreDecision {
        // The one branch that reads the receipt rather than the host: a
        // displacement the hand-off never reached is not ownership, so there is
        // nothing to hand back.
        //
        // "No host state could make there be something" used to follow, and stopped
        // being true once `plugins install` and `plugins enable` were understood to
        // perform the hand-off themselves for a same-kind plugin. The receipt is
        // marked when that is attributable, but attribution needs a slot probe, and
        // a probe the host cannot answer — or a process that dies before the mark is
        // persisted — leaves the entry unapplied over a plugin this adapter really
        // did turn off. Skipping the restore there told the operator "this adapter
        // never disabled it", reported a complete cleanup, removed the receipt, and
        // left the bundled plugin off with nothing recording why. So an unapplied
        // entry is skipped only when the host does *not* show this adapter's own
        // selection having done the work; when it does, the entry is restored like
        // any applied one and the report says so.
        if !entry.applied
            && !self.handoff_performed_by_slot_selection(
                entry,
                host.own_plugin_id,
                host.slots,
                home,
                ctx,
            )
        {
            return RestoreDecision::SkipNotApplied;
        }
        match self.displacement_block(
            &entry.plugin_id,
            host.inventory,
            home,
            ctx,
            AllowlistMeaning::RefusesRestore,
            host.allowlist,
        ) {
            DisplacementBlock::Absent => return RestoreDecision::SkipAbsent,
            DisplacementBlock::Denied => return RestoreDecision::SkipDenied,
            DisplacementBlock::AllowlistExcluded => {
                return RestoreDecision::SkipAllowlistExcluded;
            }
            DisplacementBlock::PluginsGloballyDisabled => {
                return RestoreDecision::SkipPluginsGloballyDisabled;
            }
            DisplacementBlock::None => {}
        }
        if let Some(slot) = entry.slot.as_deref() {
            // A live read, not the capture, and the difference from the branch
            // above is the question. That one asks who turned the plugin off,
            // which is a fact about the past and is destroyed by this disable's own
            // `plugins uninstall`. This one asks whether handing the plugin back
            // would take the slot from somebody who owns it *now* — and after the
            // uninstall the answer really has changed: the host reset the slot to
            // its default, an operator may have moved or closed it inside the
            // window, and `plugins enable` re-runs slot selection over whatever it
            // finds. Reasoning from the capture here would step over all three.
            //
            // For a displacement of the slot's own default plugin — the shape this
            // driver ships — the two readings agree anyway, because the reset
            // target *is* the displaced plugin and `slot_restore_decision` reads
            // that as `Proceed`.
            let answer = self.read_slot_owner(slot, home, ctx);
            return match slot_restore_decision(
                answer.as_deref(),
                host.own_plugin_id,
                &entry.plugin_id,
            ) {
                SlotRestore::Proceed => RestoreDecision::Restore,
                SlotRestore::ExplicitlyOff(sentinel) => RestoreDecision::SkipSlotClosed {
                    slot: slot.to_string(),
                    sentinel,
                },
                SlotRestore::OwnedByThird(owner) => RestoreDecision::SkipSlotOwned {
                    slot: slot.to_string(),
                    owner,
                },
            };
        }
        RestoreDecision::Restore
    }

    /// Build the dry-run lines for restoring `entries` out of the state directory
    /// `claim` names, by asking [`Self::restore_decision`] — the same call the real
    /// restore makes — so a preview cannot describe a branch the operation will
    /// not take.
    ///
    /// Read-only: the decision probes `plugins list`, the two policy keys and
    /// `plugins.slots.<slot>`, all of which are reads. Nothing is written and no
    /// receipt is touched.
    ///
    /// `allowlist` is the one input the caller has to supply rather than the host,
    /// because a preview cannot observe the uninstall it has not run: it says
    /// whether the operation being previewed removes this adapter's own plugin from
    /// `plugins.allow` before the restore it predicts. Every other reading is taken
    /// live, and for the slot that is the right answer on both sides — the veto
    /// asks who owns the slot when the `plugins enable` would run, which a preview
    /// can no more predict than the operation can, and which the caller's own
    /// capture therefore does not feed into here. See [`AllowlistReading`].
    fn restore_preview_lines(
        &self,
        claim: &AdapterClaim,
        entries: &[DisplacedPlugin],
        ctx: &DriverCtx,
        scope: &str,
        why: &str,
        allowlist: AllowlistReading<'_>,
    ) -> Result<Vec<String>, AdapterError> {
        // Resolved before the empty-entries shortcut, exactly where the callers
        // used to resolve it themselves: a receipt whose state directory cannot be
        // named is one a preview must reject rather than describe.
        let home = claim_state_dir(claim)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let own_plugin_id = claim_own_plugin(claim)?;
        let inventory = self.read_plugin_inventory(&home, ctx);
        // A preview mutates nothing, so the host it reads is the host the
        // operation will find — with the one exception `allowlist` exists for, a
        // key the operation's own uninstall rewrites before the restore reads it.
        let host = RestoreHost {
            own_plugin_id: own_plugin_id.as_deref(),
            inventory: inventory.as_deref(),
            slots: SlotReadings::Live,
            allowlist,
        };
        let mut lines = Vec::with_capacity(entries.len());
        for entry in entries {
            let decision = self.restore_decision(entry, &host, &home, ctx);
            lines.push(restore_preview_line(
                &decision,
                &entry.plugin_id,
                scope,
                why,
            ));
        }
        Ok(lines)
    }

    /// Describe the restore `cleanup_replaced_claim` performs for a same-home
    /// re-enable: every plugin the prior receipt claims as displaced that the
    /// contract being enabled no longer declares.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] from
    /// [`Self::attribute_dropped_prior_handoffs`] when the host cannot answer the
    /// attribution for an unapplied entry the operation is about to let go of, and
    /// whatever [`Self::restore_preview_lines`] propagates. Failing the plan is the
    /// point: the operation fails on the same answer, and a preview that described
    /// the restore anyway would promise a host mutation the re-enable then refuses
    /// to make.
    fn plan_dropped_displacement_restores(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let dropped: Vec<DisplacedPlugin> = claim_displaced_plugins(prior)?
            .into_iter()
            .filter(|entry| {
                !ctx.declared_displaces
                    .iter()
                    .any(|spec| spec.id == entry.plugin_id)
            })
            .collect();
        self.attribute_dropped_prior_handoffs(prior, &dropped, ctx)?;
        self.restore_preview_lines(
            prior,
            &dropped,
            ctx,
            "",
            "which the contract being enabled no longer displaces",
            // A same-home re-enable keeps the prior installation, so nothing
            // removes this adapter's own plugin from `plugins.allow` ahead of the
            // restore and the host's current key is the one the operation reads.
            AllowlistReading::Live,
        )
    }

    /// Re-enable the framework plugins this adapter displaced, honoring a slot
    /// the operator moved elsewhere in the meantime.
    ///
    /// Restores only what the receipt claims, so a plugin the operator had
    /// already disabled before enable is never re-enabled here. A failed
    /// restore reports through `cleanup_complete` rather than an error, so the
    /// Manager keeps the receipt and disable stays retryable.
    ///
    /// Each restore that *succeeds*, and each veto that finds the ownership
    /// already gone or hands it back to the operator, strikes that entry from the
    /// receipt on the spot, because this loop can fail on a later entry and its
    /// caller can fail on something unrelated afterwards — and in both cases the
    /// Manager keeps and re-persists the receipt. An entry left behind would
    /// record a transition that has already been undone, or a surrender of it
    /// that has already been announced to the operator, so a retry would perform
    /// it a second time over whatever they did in between. This is the
    /// disable-side half of the invariant `cleanup_replaced_claim` states for the
    /// re-enable side: a successful release must be followed by a durable record
    /// of it, and the driver has no way to write one except through the claim it
    /// is handed. The Manager closes the one gap the driver cannot — an error out
    /// of this loop, which reaches it as no report at all — by persisting the
    /// claim as mutated before it propagates.
    ///
    /// The single exception is an entry whose hand-off never ran. That was never
    /// ownership, so there is no release to record and no retry action to
    /// prevent; see the branch for why the entry stays.
    fn restore_displaced_plugins(
        &self,
        claim: &mut AdapterClaim,
        displaced: &[DisplacedPlugin],
        home: &Path,
        ctx: &DriverCtx,
        slots: SlotReadings<'_>,
    ) -> Result<DisableReport, AdapterError> {
        if displaced.is_empty() {
            return Ok(DisableReport {
                cleanup_complete: true,
                messages: Vec::new(),
            });
        }
        let mut messages = Vec::new();
        let mut cleanup_complete = true;
        let own_plugin_id = claim_own_plugin(claim)?;
        let inventory = self.read_plugin_inventory(home, ctx);
        // Both callers of this method stand on the same side of the uninstall:
        // `disable` restores in its step 3, after the step-2 `plugins uninstall`
        // has already rewritten the host, and the same-home `cleanup_replaced_claim`
        // branch runs no uninstall at all because a same-home re-enable keeps the
        // prior installation. So the live `plugins.allow` is the answer the restore
        // really meets, and the slot readings come from whatever capture the caller
        // took.
        let host = RestoreHost {
            own_plugin_id: own_plugin_id.as_deref(),
            inventory: inventory.as_deref(),
            slots,
            allowlist: AllowlistReading::Live,
        };
        for entry in displaced {
            // Already whitelist-validated when the references were resolved,
            // before any mutation; re-checking here keeps this helper safe to
            // call on its own.
            validate_plugin_id(&entry.plugin_id)?;
            let plugin_id = entry.plugin_id.clone();
            let decision = self.restore_decision(entry, &host, home, ctx);
            // The host can also have released this ownership, and recognizing
            // that is what keeps disable convergent. A plugin no longer in the
            // inventory has nothing to hand back; one an explicit policy keeps
            // off will refuse the `plugins enable` this restore would issue.
            // Neither is a failure — retrying can never succeed, so reporting
            // `cleanup_complete = false` would strand the receipt forever, with
            // this adapter's own plugin already uninstalled, over a cleanup that
            // has nothing left to do.
            //
            // A veto is also a *release*, and every message below says so: the
            // ownership is already gone, or it is handed back to the operator
            // with the exact command to undo it themselves. So each one strikes
            // the entry the way a successful restore does. Leaving it in a
            // receipt kept for some unrelated failure would record ownership
            // this adapter has just given up, and the retry re-decides against
            // a host that may well have moved on — the deny lifted, the slot
            // reopened, the plugin reinstalled — and then runs the very
            // `plugins enable` the earlier run told the operator was theirs to
            // run, over whatever they did in between.
            let vetoed = match &decision {
                // The one branch that is not a release: the hand-off never ran,
                // so this adapter never held the plugin and there is no
                // transition for a retry to repeat — `applied` is a fact about
                // this receipt's own past that no host state can change, so the
                // decision is stable by construction rather than by veto. The
                // entry is also the only record that the plugin is enabled *and*
                // unclaimed, which is what `status` reads as `never_displaced`;
                // striking it would delete that signal and buy nothing.
                RestoreDecision::SkipNotApplied => {
                    messages.push(format!(
                        "left openclaw plugin '{plugin_id}' alone: the enable that recorded this \
                         displacement failed before the hand-off ran, so this adapter never \
                         disabled it and there is nothing to restore"
                    ));
                    continue;
                }
                RestoreDecision::SkipAbsent => Some(format!(
                    "left openclaw plugin '{plugin_id}' alone: it is no longer in this host's \
                     `plugins list` inventory, so the displacement is already released"
                )),
                RestoreDecision::SkipDenied => Some(format!(
                    "left openclaw plugin '{plugin_id}' disabled: `plugins.deny` names it, so \
                     OpenClaw neither loads it nor accepts the restore, and the displacement is \
                     treated as released; remove it from `plugins.deny` and run `openclaw \
                     plugins enable {plugin_id}` yourself if you want it back"
                )),
                RestoreDecision::SkipAllowlistExcluded => Some(format!(
                    "left openclaw plugin '{plugin_id}' disabled: a restrictive `plugins.allow` \
                     excludes it, so OpenClaw gates the restore on that key and this driver will \
                     not edit the operator's allowlist to force it; the displacement is treated \
                     as released. Add it to `plugins.allow` and run `openclaw plugins enable \
                     {plugin_id}` yourself if you want it back"
                )),
                RestoreDecision::SkipPluginsGloballyDisabled => Some(format!(
                    "left openclaw plugin '{plugin_id}' disabled: `plugins.enabled` is false, \
                     so OpenClaw refuses every `plugins enable` and would refuse this \
                     restore; the displacement is treated as released. Set `plugins.enabled` \
                     back to true and run `openclaw plugins enable {plugin_id}` yourself if \
                     you want it back"
                )),
                RestoreDecision::SkipSlotClosed { slot, sentinel } => Some(format!(
                    "left openclaw plugin '{plugin_id}' disabled: plugins.slots.{slot} is \
                     explicitly '{sentinel}', which closes the slot; re-enabling the plugin \
                     would re-run OpenClaw's slot selection and silently undo that choice. \
                     Run `openclaw plugins enable {plugin_id}` yourself if you want it back"
                )),
                RestoreDecision::SkipSlotOwned { slot, owner } => Some(format!(
                    "left openclaw plugin '{plugin_id}' disabled: plugins.slots.{slot} now \
                     belongs to '{owner}', which was selected after this adapter displaced \
                     '{plugin_id}'; run `openclaw plugins enable {plugin_id}` yourself if \
                     that is what you want"
                )),
                RestoreDecision::Restore => None,
            };
            if let Some(message) = vetoed {
                release_displacement_claim(claim, &entry.resource)?;
                messages.push(message);
                continue;
            }
            // `restore_decision` already stepped aside for a third owner and for
            // an explicitly closed slot; see its doc for why an empty or
            // unanswerable slot read keeps the restore.
            //
            // No `--accept-capabilities` here: the displaced plugin is a
            // bundled one this adapter is handing *back*, not a third-party
            // bundle whose declared capabilities the caller consented to.
            let cmd = build_enable_cmd(&plugin_id, home, ctx.user_home.as_deref(), false);
            let output = ctx.ops.run_framework_cli(cmd)?;
            if output.success() {
                // Strike the ownership from the receipt the moment the host
                // reflects it, not at the end of the whole cleanup. This loop can
                // fail on a *later* entry, and `disable` can fail on something
                // unrelated after it — either way the Manager keeps the receipt and
                // re-persists it, so an entry left behind here would record a
                // transition that has already been undone. A retry would then
                // perform it a second time, re-enabling a plugin the operator may
                // well have closed themselves in between, and nothing would show
                // why. Removing it is what keeps the kept receipt describing the
                // host as it actually is.
                release_displacement_claim(claim, &entry.resource)?;
                messages.push(format!(
                    "re-enabled openclaw plugin '{plugin_id}', which this adapter had displaced"
                ));
            } else {
                cleanup_complete = false;
                messages.push(format!(
                    "failed to re-enable displaced openclaw plugin '{plugin_id}': {}; the \
                     receipt is kept so disable can be retried",
                    cli_failure_reason("plugins enable", &output)
                ));
            }
        }
        Ok(DisableReport {
            cleanup_complete,
            messages,
        })
    }

    /// Best-effort read of `plugins.slots.<slot>`: `None` when the host cannot
    /// answer at all, `Some(token)` — possibly empty — when it did.
    ///
    /// The two are kept apart because they mean different things downstream: a
    /// probe the host cannot answer is no evidence about the operator's choice,
    /// while a readable answer can be an explicit "slot off" that a restore must
    /// not undo.
    fn read_slot_owner(&self, slot: &str, home: &Path, ctx: &DriverCtx) -> Option<String> {
        let key = format!("plugins.slots.{slot}");
        let cmd = build_config_get_cmd(&key, home, ctx.user_home.as_deref());
        let output = ctx.ops.run_framework_cli(cmd).ok()?;
        if !output.success() {
            return None;
        }
        Some(config_answer_token(&output))
    }

    /// Read the slots of the displacements whose hand-off was never *marked*,
    /// before anything this operation runs can rewrite them. See
    /// [`SlotReadings::Captured`].
    ///
    /// Only unapplied entries, because only they have a question to ask about the
    /// past: [`Self::handoff_attribution`] is the one decision that
    /// reads the slot as evidence of who turned the plugin off, and everything else
    /// reads it as a statement about right now. An applied entry's slot veto keeps
    /// its live read on purpose — see the branch in [`Self::restore_decision`].
    ///
    /// One read per distinct slot, not per entry: two entries cannot share a slot
    /// (`claim_displaced_plugins` rejects that). Probe count is unchanged overall,
    /// because an unapplied entry that gets restored used to read its slot twice —
    /// once for the attribution and once for the veto — and now reads it once here
    /// and once there. [`Self::record_recovered_slot_selection_handoffs`] is the
    /// capture's other consumer and asks the same question over the same readings,
    /// after which `disable` re-resolves the receipt and the restore reads the mark
    /// instead of re-attributing it; only an entry nothing can attribute is probed
    /// twice, and it is on the path that restores nothing.
    fn capture_slot_readings(
        &self,
        displaced: &[DisplacedPlugin],
        home: &Path,
        ctx: &DriverCtx,
    ) -> CapturedSlots {
        let mut readings = CapturedSlots::new();
        for entry in displaced {
            if entry.applied {
                continue;
            }
            let Some(slot) = entry.slot.as_deref() else {
                continue;
            };
            let reading = self.read_slot_owner(slot, home, ctx);
            readings.entry(slot.to_string()).or_insert(reading);
        }
        readings
    }

    /// Write the hand-off attributions a `disable` recovered from its capture into
    /// the receipt, before anything it runs can destroy the evidence for them.
    ///
    /// [`Self::record_slot_selection_handoffs`] is the enable-side half of the same
    /// duty and asks the same question through the same predicate. The two differ
    /// only in where the slot half of the answer comes from, and that difference is
    /// why this is a separate step rather than a call to that one: enable reads a host
    /// nothing has mutated yet, so it reads live, while a `disable` has to reason from
    /// [`Self::capture_slot_readings`] because its own `plugins uninstall` resets the
    /// slot to the host default. Whatever the capture recovered therefore exists only
    /// in this call's stack frame — after the uninstall the host holds nothing that
    /// would reproduce it.
    ///
    /// Recovering it without recording it leaves the recovery path non-retryable, and
    /// the report does not say so. A restore that fails keeps the receipt and advises
    /// a retry; the retry then captures a slot naming the plugin the uninstall put
    /// back, [`Self::restore_decision`] answers
    /// [`RestoreDecision::SkipNotApplied`], and disable reports a complete cleanup and
    /// removes the receipt over a bundled plugin still switched off. That is the
    /// stranded host the capture exists to prevent, reached one attempt later with
    /// nothing done in between — so persisting the recovery is what makes the advice
    /// in that message true.
    ///
    /// Marking before the uninstall rather than after the restore is the write-ahead
    /// ordering [`Self::apply_displacements`] uses, for the same reason: the
    /// attribution is a fact about the past, so its durable record must not depend on
    /// the steps that follow succeeding. A crash anywhere in this disable then leaves
    /// a receipt describing an ownership the host really established.
    ///
    /// Durable means written by this call, which is why it takes `progress` and
    /// saves each mark as it makes it — the same pairing
    /// [`Self::record_slot_selection_handoffs`] uses on the enable side. The
    /// Manager does re-persist the receipt on every path that keeps it, the error
    /// one included, but each of those saves runs only once `disable` has returned,
    /// and the uninstall sitting in between is precisely the step that erases the
    /// reading the attribution was recovered from. A process killed inside that
    /// window — SIGKILL, OOM, power loss — would leave a slot naming the plugin
    /// the uninstall put back over a receipt still reading `applied = false`, and
    /// the retry then has nothing left to recover from: it reports this adapter
    /// never disabled the plugin, drops the receipt, and strands the host. So the
    /// recovery is what has to reach the disk before the host mutation, not
    /// merely before the end of the call.
    ///
    /// Positive evidence only, exactly as on the enable side: an entry the host
    /// rules out stays unapplied, and a capture the host could not answer stays
    /// unattributed rather than becoming a guess.
    ///
    /// "Could not answer" is an error here and a verdict everywhere else, and the
    /// difference is what this disable does next. Every other reader of the
    /// attribution has *inaction* for its negative direction, so folding `Unknown`
    /// into "not ours" costs nothing a later attempt cannot undo. This one is
    /// followed by the rest of the disable, and the rest of the disable is what
    /// destroys the question: `plugins uninstall` resets the slot the attribution
    /// reads to the host default, the restore then skips an entry nothing marked,
    /// the cleanup reports itself complete, and the Manager removes the one receipt
    /// that ever named the hand-off. A single failed `config get` inside the very
    /// capture this method exists to take therefore used to end with the bundled
    /// plugin off, no record of who turned it off, and nothing left to recover from
    /// — the stranded host both the capture and the write-ahead mark were added to
    /// prevent, reached with no crash involved and a report claiming this adapter
    /// never disabled the plugin. Failing first costs one retry and keeps both
    /// halves of the evidence: the host still holds the slot reading the capture
    /// missed, and the receipt still names the displacement.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the host cannot answer the attribution of
    /// an unapplied entry, i.e. on [`HandoffAttribution::Unknown`] — raised before
    /// this disable's first mutation, so the host and the receipt both survive for
    /// the retry. Also propagates a receipt-consistency error from
    /// [`claim_own_plugin`] or [`mark_displacement_applied`], both meaning the
    /// caller's view of the receipt has diverged from the receipt itself, and a
    /// persistence failure from `progress` — which aborts the disable with the host
    /// untouched rather than proceeding to destroy evidence the receipt never
    /// recorded.
    fn record_recovered_slot_selection_handoffs(
        &self,
        claim: &mut AdapterClaim,
        displaced: &[DisplacedPlugin],
        slots: &CapturedSlots,
        home: &Path,
        ctx: &DriverCtx,
        progress: &mut dyn ClaimProgress,
    ) -> Result<(), AdapterError> {
        let own_plugin_id = claim_own_plugin(claim)?;
        for entry in displaced {
            // Already recorded, and skipped by the capture as well, so there is no
            // reading here to recover an attribution from and nothing to write.
            if entry.applied {
                continue;
            }
            if !self.recovered_disable_attribution(
                entry,
                own_plugin_id.as_deref(),
                SlotReadings::Captured(slots),
                home,
                ctx,
            )? {
                continue;
            }
            mark_displacement_applied(claim, &entry.resource)?;
            progress.persist_claim(claim)?;
        }
        Ok(())
    }

    /// Whether a `disable` recovers one unapplied entry's hand-off from the evidence
    /// it holds — and the failure that stops the whole disable when the host will
    /// not answer.
    ///
    /// [`Self::handoff_attribution`] with its `Unknown` turned into an error, kept in
    /// one place because two callers have to agree on it and they are the two halves
    /// of one promise: [`Self::record_recovered_slot_selection_handoffs`] decides what
    /// the write-ahead mark carries, and [`Self::plan_disable_restores`] decides what
    /// the preview says about the disable that mark feeds. A preview reading "left
    /// alone, this adapter never disabled it" over a disable that refuses to run at
    /// all is the contradiction [`Self::recovered_prior_handoff`] exists to rule out
    /// on the enable side, arriving in the other direction — and it is invisible
    /// afterwards, because the operator who reads the plan and then runs the command
    /// gets an error instead of either outcome.
    ///
    /// `slots` differs per caller for the reason [`SlotReadings`] gives: the real
    /// disable reasons from the capture its own uninstall is about to invalidate,
    /// while a preview has no capture of its own and reads the host as it stands.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] on [`HandoffAttribution::Unknown`]. Neither
    /// caller has mutated anything by the time it asks, so the failure leaves the
    /// host and the receipt exactly as they were and the retry gets to ask again.
    fn recovered_disable_attribution(
        &self,
        entry: &DisplacedPlugin,
        own_plugin_id: Option<&str>,
        slots: SlotReadings<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> Result<bool, AdapterError> {
        match self.handoff_attribution(entry, own_plugin_id, slots, home, ctx) {
            HandoffAttribution::Performed => Ok(true),
            HandoffAttribution::NotPerformed => Ok(false),
            HandoffAttribution::Unknown => Err(unreadable_handoff_evidence_on_disable(
                ctx,
                &entry.plugin_id,
            )),
        }
    }

    /// The read-only gate every preview of a `disable` shares with the operation:
    /// each unapplied entry the receipt names must have an attribution the host can
    /// actually answer before anything describes — or performs — the restore that
    /// follows it.
    ///
    /// One function for two callers because they have to agree, and they are not the
    /// two callers of one operation: [`Self::plan_disable_restores`] previews a
    /// disable of the current receipt in its own home, while
    /// [`Self::plan_reenable_cleanup`] previews the *cross-home* re-enable cleanup,
    /// whose operation is a full `disable` of the prior receipt against the prior
    /// home. That second one is easy to miss precisely because it is a re-enable, and
    /// it was missed: the plan listed the prior home's uninstall and its restores
    /// while the operation refused to run at all. The operation's own gate is
    /// [`Self::record_recovered_slot_selection_handoffs`], which asks the same
    /// question of its pre-uninstall capture instead of of the live host — the only
    /// difference between them, and the one [`SlotReadings`] exists to state.
    ///
    /// Reads live because a preview has no capture of its own, and stands before the
    /// host has been touched, so the live reading is the one the capture will take.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] from [`Self::recovered_disable_attribution`] on
    /// the first entry whose attribution is [`HandoffAttribution::Unknown`], and a
    /// receipt-consistency error from [`claim_own_plugin`] or [`claim_state_dir`].
    /// Both callers are plans, so nothing has run and the retry asks the same
    /// question of an unchanged host.
    fn gate_disable_restores_on_attribution(
        &self,
        claim: &AdapterClaim,
        displaced: &[DisplacedPlugin],
        ctx: &DriverCtx,
    ) -> Result<(), AdapterError> {
        if displaced.iter().all(|entry| entry.applied) {
            return Ok(());
        }
        let own_plugin_id = claim_own_plugin(claim)?;
        let home = claim_state_dir(claim)?;
        for entry in displaced {
            if entry.applied {
                continue;
            }
            self.recovered_disable_attribution(
                entry,
                own_plugin_id.as_deref(),
                SlotReadings::Live,
                &home,
                ctx,
            )?;
        }
        Ok(())
    }

    /// The `plugins.slots.<slot>` token to reason from, from a capture when the
    /// caller has one and from the host when it does not.
    fn slot_owner(
        &self,
        slot: &str,
        slots: SlotReadings<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> Option<String> {
        match slots {
            SlotReadings::Live => self.read_slot_owner(slot, home, ctx),
            // A slot the capture does not cover falls back to a live read. The
            // capture is built from the same entries these decisions run over, so
            // nothing reaches this branch; it is here so a caller that captured a
            // subset degrades to the old behaviour instead of to a wrong verdict.
            SlotReadings::Captured(readings) => match readings.get(slot) {
                Some(reading) => reading.clone(),
                None => self.read_slot_owner(slot, home, ctx),
            },
        }
    }

    /// Whether this driver's own slot selection is a sufficient explanation for a
    /// displaced plugin's enablement flag reading `false`.
    ///
    /// OpenClaw applies exclusive slot selection on both verbs this enable runs
    /// before the hand-off — `plugins install` (`plugins-install-persist.ts`) and
    /// `plugins enable` (`plugins-cli.ts`) in v2026.4.14, both through
    /// `applySlotSelectionForPlugin`. Selecting a plugin writes two things: the slot
    /// for its kind comes to name it, and every *other* plugin of that kind that
    /// does not own another slot is set `enabled = false`. `memory-anolisa` and
    /// `memory-core` both declare `kind = "memory"`, so installing and enabling this
    /// adapter's own plugin turns the bundled one off by itself — no operator, no
    /// framework update.
    ///
    /// The second write is not the only trace of it, and that is what makes the
    /// state attributable at all: the same call records the slot as naming the plugin
    /// it selected. So a `false` flag *together with* the entry's slot reading as
    /// this adapter's own plugin is a state this driver produces by construction, and
    /// the claim stays — releasing on it would delete the only record that the
    /// bundled plugin is off, the receipt would claim no displacement, disable would
    /// report a complete cleanup and remove it, and the host would be left with no
    /// memory backend at all and nothing that would ever restore one.
    ///
    /// A `false` flag *without* that corroboration is not this driver's doing — an
    /// adapter whose own plugin declares no slot kind writes no slot at all, and a
    /// slot naming anybody else was not selected here — so it keeps the older
    /// reading and releases the claim, which is what stops a later disable from
    /// re-opening a plugin the operator closed themselves.
    ///
    /// "Without" means a readable answer that names somebody else, and only that. A
    /// probe the host cannot answer is a third state, kept apart from both: it is
    /// not corroboration, but it is not evidence of an operator either, and the two
    /// directions are not symmetric. Collapsing it into "not ours" turned one
    /// transient `config get` failure into a destructive ownership decision — the
    /// claim was deleted, the receipt then named no displacement, disable reported a
    /// complete cleanup and removed it, and the bundled plugin stayed off with
    /// nothing recording why. That is the same rule the re-confirm already states
    /// for the enablement probe and that `status` states by keeping `unreadable` a
    /// bucket of its own: could-not-read says nothing about who did it, so the claim
    /// stays and the retry gets another chance to ask.
    ///
    /// Residual, stated plainly: an operator who had already pointed the slot at this
    /// adapter's own plugin and then disabled the plugin inside the window is
    /// indistinguishable from this driver's own selection. That costs one
    /// `plugins enable` at disable time, announced in that report; the opposite
    /// error costs a stranded host, which this driver ranks as worse everywhere else.
    fn slot_attribution(
        &self,
        entry: &DisplacedPlugin,
        own_plugin_id: Option<&str>,
        slots: SlotReadings<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> SlotAttribution {
        // No slot in the receipt is an answer, not a failed question: see the
        // variant. A receipt with no plugin of its own cannot be attributed either
        // way, and `apply_enable` rejects that shape before it reaches here.
        let Some(slot) = entry.slot.as_deref() else {
            return SlotAttribution::NotDeclared;
        };
        let Some(own) = own_plugin_id else {
            return SlotAttribution::Unreadable;
        };
        match self.slot_owner(slot, slots, home, ctx) {
            Some(token) if token == own => SlotAttribution::Own,
            // Readable and not ours — another plugin, the displaced plugin itself,
            // an explicit off sentinel, or no selection recorded at all.
            Some(_) => SlotAttribution::Other,
            None => SlotAttribution::Unreadable,
        }
    }

    /// Whether the host shows this adapter's own slot selection having *already*
    /// performed one of its displacement hand-offs, and — when it cannot say —
    /// that it cannot say.
    ///
    /// Both halves are required, and each rules out a different mistake. The slot
    /// naming this adapter's own plugin says its selection ran — `plugins install`
    /// and `plugins enable` both run it — but selection only disables plugins of the
    /// selected kind, so the plugin's own flag reading off is what says *this* entry
    /// was one of them. One shared question, because every caller has to agree:
    /// the write-ahead mark in [`Self::record_slot_selection_handoffs`], the one
    /// [`Self::record_recovered_slot_selection_handoffs`] writes when a `disable`
    /// recovers the attribution the mark missed, the restore branch in
    /// [`Self::restore_decision`], the bucket `status` files the entry under, and
    /// the re-enable that has to tell an unperformed hand-off from an unrecorded
    /// one. They disagreed once already, and the disagreement was visible to the
    /// operator as a report saying "this adapter never disabled it" from the same
    /// run that was about to re-enable it. They disagreed a second time across a
    /// re-enable, where the restore branch handed the plugin back on exactly this
    /// evidence while the replacement receipt declined to carry it — see
    /// [`prior_unrecorded_displacements`].
    ///
    /// `slots` is where the slot half comes from, and it is the only thing the
    /// callers do not share: the enable-side mark and `status` read the host live,
    /// while both `disable`-side callers reason from the readings that disable
    /// captured before its own `plugins uninstall` reset the slot — see
    /// [`SlotReadings::Captured`]. The enablement half stays a live read
    /// everywhere, because nothing this driver runs writes another plugin's flag:
    /// `removePluginFromConfig` drops the *uninstalled* plugin's own entry and
    /// leaves the displaced one alone.
    fn handoff_attribution(
        &self,
        entry: &DisplacedPlugin,
        own_plugin_id: Option<&str>,
        slots: SlotReadings<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> HandoffAttribution {
        match self.slot_attribution(entry, own_plugin_id, slots, home, ctx) {
            // The host named somebody else, or the contract declared no slot for
            // this entry to compete for one. Both are answers, and both rule this
            // adapter out.
            SlotAttribution::Other | SlotAttribution::NotDeclared => {
                return HandoffAttribution::NotPerformed;
            }
            // Neither half carries the attribution on its own, so an unreadable
            // slot is an unreadable verdict however the flag reads.
            SlotAttribution::Unreadable => return HandoffAttribution::Unknown,
            SlotAttribution::Own => {}
        }
        match self.read_plugin_enablement(&entry.plugin_id, home, ctx) {
            PluginEnablement::Disabled => HandoffAttribution::Performed,
            // Positively on: whatever the slot says, there is no hand-off in force
            // to recover and no ownership to carry.
            PluginEnablement::Enabled => HandoffAttribution::NotPerformed,
            PluginEnablement::Unknown => HandoffAttribution::Unknown,
        }
    }

    /// [`Self::handoff_attribution`] collapsed to its positive answer.
    ///
    /// Right for every caller whose negative direction is *inaction* — leaving an
    /// entry unapplied or skipping a restore, where the next attempt asks the same
    /// question of an unchanged host. There, "the host rules this adapter out" and
    /// "the host could not say" lead to the same place, and a read failure must keep
    /// leading there: marking a hand-off on evidence nobody could read would claim a
    /// transition this adapter never made.
    ///
    /// Two callers are not like that, and both ask the tri-state directly instead.
    /// A re-enable's negative direction drops an ownership the prior receipt is the
    /// only record of — see [`Self::recovered_prior_handoff`]. A `disable`'s is worse
    /// still, because what follows it destroys the evidence the question was asked of
    /// and then removes the receipt — see
    /// [`Self::recovered_disable_attribution`].
    fn handoff_performed_by_slot_selection(
        &self,
        entry: &DisplacedPlugin,
        own_plugin_id: Option<&str>,
        slots: SlotReadings<'_>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> bool {
        matches!(
            self.handoff_attribution(entry, own_plugin_id, slots, home, ctx),
            HandoffAttribution::Performed
        )
    }

    /// Whether a re-enable recovers an ownership the prior receipt recorded but
    /// never marked as performed.
    ///
    /// One function rather than a copy per caller because its first two callers are
    /// the two halves of one promise: [`Self::prepare_enable`] decides what the
    /// replacement receipt carries, and [`Self::plan_enable`] decides what the
    /// preview says about it. They disagreed, and the disagreement was the worst
    /// kind — a dry-run reading "leave it alone, it is not claimed and disable will
    /// not re-enable it" over an enable whose receipt carried the hand-off as
    /// performed and whose disable then really did re-enable the plugin. An operator
    /// assessing what a later uninstall would cost got the opposite of the truth
    /// from the one command whose job is to tell them.
    ///
    /// The judgment itself is [`Self::prior_handoff_recovery`], and this function is
    /// its map lookup. One further caller asks the judgment directly:
    /// [`Self::attribute_dropped_prior_handoffs`], which gates an entry the
    /// replacement receipt does not carry at all — a displacement the contract being
    /// enabled dropped. It already holds its entries and consumes no verdict, only
    /// the failure, so it has nothing to look them up in.
    ///
    /// Asked only for a plugin a prior receipt *for this same instance* already
    /// names as unapplied — `unrecorded` is [`prior_unrecorded_displacements`], so
    /// it is empty on a first enable and across a migration, and both callers get
    /// [`PriorHandoffRecovery::NothingToRecover`] without a single host read. That
    /// gate is what keeps the recovery from becoming the unconditional inheritance
    /// this branch exists to avoid: an operator's own disable still reads as theirs,
    /// because on a first enable there is no older record to recover and a slot
    /// naming this adapter's plugin can mean nothing more than that somebody
    /// installed it by hand and closed the bundled one themselves.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::prior_handoff_recovery`]'s, which is the whole of this
    /// function's own contract: the lookup it adds cannot fail, and an entry the map
    /// does not name is a *verdict* — [`PriorHandoffRecovery::NothingToRecover`],
    /// with no host read at all — rather than an error.
    fn recovered_prior_handoff(
        &self,
        unrecorded: &BTreeMap<String, DisplacedPlugin>,
        displaced_id: &str,
        own_plugin_id: Option<&str>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> Result<PriorHandoffRecovery, AdapterError> {
        let Some(entry) = unrecorded.get(displaced_id) else {
            return Ok(PriorHandoffRecovery::NothingToRecover);
        };
        self.prior_handoff_recovery(entry, own_plugin_id, home, ctx)
    }

    /// [`Self::handoff_attribution`] with its `Unknown` turned into an error, on the
    /// re-enable side — the counterpart of [`Self::recovered_disable_attribution`],
    /// and the whole of what [`Self::recovered_prior_handoff`] does once its map
    /// lookup has answered. Split out because a caller that already holds the entry
    /// should not have to index a map to ask about it.
    ///
    /// Reads the slot live rather than from a capture, which is the difference the
    /// two counterparts have and the one [`SlotReadings`] states: nothing on the
    /// re-enable side runs behind an uninstall that would invalidate a reading, so
    /// there is no capture to preserve and the live host is the correct one to ask.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the host cannot answer the attribution,
    /// i.e. on [`HandoffAttribution::Unknown`]. Failing is the only answer that
    /// keeps both directions honest: recovering on a read failure would claim a
    /// disable the operator made themselves, and declining on one would drop the
    /// only record of a disable this adapter made — after which the same-home
    /// cleanup hands the plugin back, this enable's own install switches it off
    /// again, and the disable that follows reports a receipt naming nothing to
    /// restore. Every caller raises it before any mutation, so the prior receipt
    /// survives untouched and the retry gets to ask again.
    fn prior_handoff_recovery(
        &self,
        entry: &DisplacedPlugin,
        own_plugin_id: Option<&str>,
        home: &Path,
        ctx: &DriverCtx,
    ) -> Result<PriorHandoffRecovery, AdapterError> {
        match self.handoff_attribution(entry, own_plugin_id, SlotReadings::Live, home, ctx) {
            HandoffAttribution::Performed => Ok(PriorHandoffRecovery::Recovered),
            HandoffAttribution::NotPerformed => Ok(PriorHandoffRecovery::NothingToRecover),
            HandoffAttribution::Unknown => Err(unreadable_handoff_evidence(ctx, &entry.plugin_id)),
        }
    }

    /// Whether a same-home re-enable may let go of the displacements its replacement
    /// receipt no longer claims — and the failure that stops it when the host cannot
    /// answer for one of them.
    ///
    /// [`Self::prior_handoff_recovery`] asked of every *unapplied* entry in
    /// `dropped`, which is the set [`dropped_displaced_plugins`] leaves once the
    /// replacement receipt is known. Both of its verdicts are safe to act on here and
    /// neither is consumed: an entry the host positively rules out is the one
    /// [`Self::restore_displaced_plugins`] honestly reports as never performed, and
    /// an entry it attributes to this adapter is the one it honestly hands back. The
    /// third answer is the reason this exists.
    ///
    /// For a *dropped* entry, "skip the restore" is not the inaction it is everywhere
    /// else. The Manager's next durable action is to replace the prior receipt with
    /// one that names no such plugin, and nothing puts the entry back into it. So the
    /// skip is the last word on the ownership: it deletes the only record that this
    /// adapter turned the plugin off, over a host that still shows the plugin off,
    /// and reports `cleanup_complete` while doing it. Spending
    /// [`HandoffAttribution::Unknown`] that way needs one
    /// transient `config get` failure and nothing else — no crash, no operator — and
    /// leaves the retry with no receipt to retry from, which is the loss
    /// [`Self::recovered_prior_handoff`] was added to block arriving through the one
    /// path that does not ask it: `prepare_enable` and `plan_enable` both walk the
    /// *current* contract, so a displacement that contract dropped is invisible to
    /// them however unapplied the prior receipt leaves it, and
    /// [`preserve_openclaw_displaced_facts`] carries an entry into the replacement
    /// only when that contract still declares it *and* the prior receipt marks it
    /// applied — so neither route into the new receipt is open to this one either.
    ///
    /// Reads live, and may: nothing has mutated the host when either caller asks,
    /// because a same-home re-enable keeps the prior installation and this runs
    /// before `apply_enable`. That is also what makes the failure recoverable rather
    /// than merely honest.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] from [`Self::prior_handoff_recovery`] on the
    /// first unapplied entry whose attribution is [`HandoffAttribution::Unknown`],
    /// and a receipt-consistency error from [`claim_own_plugin`] or
    /// [`claim_state_dir`]. [`Self::cleanup_replaced_claim`] answers the first by
    /// propagating it, which is how the Manager learns to keep the prior receipt
    /// durable and fail the re-enable; [`Self::plan_dropped_displacement_restores`]
    /// is a plan, so nothing had run either way.
    fn attribute_dropped_prior_handoffs(
        &self,
        prior: &AdapterClaim,
        dropped: &[DisplacedPlugin],
        ctx: &DriverCtx,
    ) -> Result<(), AdapterError> {
        if dropped.iter().all(|entry| entry.applied) {
            return Ok(());
        }
        let own_plugin_id = claim_own_plugin(prior)?;
        let home = claim_state_dir(prior)?;
        for entry in dropped {
            if entry.applied {
                continue;
            }
            self.prior_handoff_recovery(entry, own_plugin_id.as_deref(), &home, ctx)?;
        }
        Ok(())
    }

    /// Run `openclaw plugins list` and decide whether `plugin_id` is still
    /// registered. Returns `(plugin_condition, verification_condition,
    /// plugin_registered_status)`.
    fn plugin_registered_condition(
        &self,
        plugin_id: &str,
        ctx: &DriverCtx,
    ) -> (AdapterCondition, AdapterCondition, ConditionStatus) {
        let plugin_ref = Some(ClaimResourceRef {
            id: RES_PLUGIN.to_string(),
        });
        let home = match openclaw_home(ctx.user_home.as_deref()) {
            Some(h) => h,
            None => {
                return (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: ConditionStatus::Unknown,
                        reason: Some("cannot resolve openclaw home".to_string()),
                        resource: plugin_ref,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::False,
                        reason: Some("openclaw home unresolved".to_string()),
                        resource: None,
                    },
                    ConditionStatus::Unknown,
                );
            }
        };
        let cmd = build_list_cmd(&home, ctx.user_home.as_deref());
        match ctx.ops.run_framework_cli(cmd) {
            Ok(output) if output.success() => {
                let registered = list_contains_plugin(&output.stdout, plugin_id);
                (
                    AdapterCondition {
                        kind: AdapterConditionKind::PluginRegistered,
                        status: bool_status(registered),
                        reason: (!registered)
                            .then(|| "plugin not present in `plugins list`".to_string()),
                        resource: plugin_ref,
                    },
                    AdapterCondition {
                        kind: AdapterConditionKind::VerificationSupported,
                        status: ConditionStatus::True,
                        reason: None,
                        resource: None,
                    },
                    bool_status(registered),
                )
            }
            // The list probe ran but failed, or could not spawn: we cannot
            // verify. Report Unknown, never a faked healthy/absent.
            Ok(_) | Err(_) => (
                AdapterCondition {
                    kind: AdapterConditionKind::PluginRegistered,
                    status: ConditionStatus::Unknown,
                    reason: Some("`plugins list` did not return a usable result".to_string()),
                    resource: plugin_ref,
                },
                AdapterCondition {
                    kind: AdapterConditionKind::VerificationSupported,
                    status: ConditionStatus::False,
                    reason: Some("`plugins list` unavailable".to_string()),
                    resource: None,
                },
                ConditionStatus::Unknown,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Host profile (read-only probing) and enable gating
// ---------------------------------------------------------------------------

/// Read-only pre-install facts about the installed OpenClaw CLI, gathered by
/// [`OpenClawDriver::host_profile`] from version and install/enable/inspect
/// help probes before the first mutation. Never persisted into a receipt.
#[derive(Debug, Clone)]
struct OpenClawHostProfile {
    /// Parsed `openclaw --version`, when the output was recognizable.
    version: Option<OpenClawVersion>,
    /// Trimmed raw `--version` output, for diagnostics.
    version_display: String,
    /// `openclaw plugins install --help` exposes `--force`.
    supports_install_force: bool,
    /// `openclaw plugins install --help` exposes `--accept-capabilities`.
    supports_accept_capabilities: bool,
    /// `plugins enable --help` exposes `--accept-capabilities`.
    supports_enable_accept_capabilities: bool,
    /// `openclaw plugins install --help` exposes
    /// `--dangerously-force-unsafe-install`, including whether the advertised
    /// option is still effective or has become a deprecated no-op.
    unsafe_install_support: UnsafeInstallSupport,
    /// `openclaw plugins inspect --help` exposes `--json`.
    supports_inspect_json: bool,
    /// `openclaw plugins inspect --help` exposes `--runtime`.
    supports_inspect_runtime: bool,
}

/// Effective semantics of OpenClaw's unsafe-install compatibility option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsafeInstallSupport {
    Unsupported,
    Effective,
    DeprecatedNoOp,
}

impl UnsafeInstallSupport {
    fn is_effective(self) -> bool {
        self == Self::Effective
    }
}

/// A plugin adapter's resolved read-only preflight, derived before the first
/// framework mutation and consumed by `prepare_enable`/`plan_enable`: the
/// selected config, the single install command, and the install/verify
/// capabilities to hand forward as [`PreparedEnable`].
struct PluginPreflight<'a> {
    /// The host's installer supports accepting declared plugin capabilities.
    supports_accept_capabilities: bool,
    /// `plugins enable --help` exposes `--accept-capabilities`.
    supports_enable_accept_capabilities: bool,
    /// The host's `plugins install --help` exposes the unsafe flag.
    supports_unsafe_install: bool,
    /// The host's `plugins inspect --help` exposes `--json`.
    supports_inspect_json: bool,
    /// The host's `plugins inspect --help` exposes `--runtime`.
    supports_inspect_runtime: bool,
    /// Selected config entries as `(original manifest index, spec)`. The
    /// index anchors the receipt's `openclaw_config_<i>` resource id so
    /// `apply_enable` can execute exactly the selected entries even when two
    /// entries share a config key.
    selected_config: Vec<(usize, &'a AdapterConfigSetSpec)>,
    install_cmd: FrameworkCommand,
}

impl OpenClawDriver {
    /// Run one read-only probe command, failing closed on timeout **or a
    /// non-zero exit**, and return its combined stdout/stderr on success. A
    /// failed probe must never be mistaken for a capability answer (e.g. an
    /// error message mentioning `--force`), so the exit status is checked
    /// before the output is interpreted; the diagnostics are carried in the
    /// error.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the probe cannot spawn, times out,
    /// or exits non-zero.
    fn run_read_probe(
        &self,
        ctx: &DriverCtx,
        cmd: FrameworkCommand,
        label: &str,
    ) -> Result<String, AdapterError> {
        let out = ctx.ops.run_framework_cli(cmd)?;
        if out.timed_out || !out.success() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: full_failure_reason(label, &out),
            });
        }
        Ok(combine_output(&out))
    }

    /// Probe `openclaw --version` read-only, failing closed on timeout or a
    /// non-zero exit. The version may still be unparseable (`None`); callers
    /// that require it enforce that separately.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the probe cannot spawn, times out,
    /// or exits non-zero.
    fn probe_version(
        &self,
        ctx: &DriverCtx,
    ) -> Result<(Option<OpenClawVersion>, String), AdapterError> {
        let home = require_home(ctx)?;
        let text = self.run_read_probe(
            ctx,
            build_version_cmd(&home, ctx.user_home.as_deref()),
            "openclaw --version",
        )?;
        let version = parse_openclaw_version_output(&text);
        // Keep the full trimmed output (bounded by the Manager's capture cap)
        // so a diagnostic that precedes the version line does not hide the
        // real, unparseable version text in the error.
        Ok((version, text.trim().to_string()))
    }

    /// Probe the host CLI read-only for every pre-install fact: version,
    /// `plugins install/enable/inspect --help`. All probing
    /// happens here — before the first mutation — and the inspect
    /// capabilities are carried forward to `apply_enable` as
    /// [`PreparedEnable`] so apply never re-probes. Fails closed when a probe
    /// times out or exits non-zero.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when a probe command cannot be spawned,
    /// times out, or exits non-zero.
    fn host_profile(&self, ctx: &DriverCtx) -> Result<OpenClawHostProfile, AdapterError> {
        let home = require_home(ctx)?;
        let user_home = ctx.user_home.as_deref();

        let (version, version_display) = self.probe_version(ctx)?;

        let install_help = self.run_read_probe(
            ctx,
            build_install_help_cmd(&home, user_home),
            "openclaw plugins install --help",
        )?;
        let supports_install_force = help_lists_flag(&install_help, "--force");
        let supports_accept_capabilities = help_lists_flag(&install_help, "--accept-capabilities");
        let unsafe_install_support = unsafe_install_support(&install_help);

        let enable_help = self.run_read_probe(
            ctx,
            build_enable_cmd("--help", &home, user_home, false),
            "openclaw plugins enable --help",
        )?;
        let supports_enable_accept_capabilities =
            help_lists_flag(&enable_help, "--accept-capabilities");

        let inspect_help = self.run_read_probe(
            ctx,
            build_inspect_help_cmd(&home, user_home),
            "openclaw plugins inspect --help",
        )?;
        let supports_inspect_json = help_lists_flag(&inspect_help, "--json");
        let supports_inspect_runtime = help_lists_flag(&inspect_help, "--runtime");

        Ok(OpenClawHostProfile {
            version,
            version_display,
            supports_install_force,
            supports_accept_capabilities,
            supports_enable_accept_capabilities,
            unsafe_install_support,
            supports_inspect_json,
            supports_inspect_runtime,
        })
    }

    /// Enforce the adapter-level framework version requirement against a
    /// probed version. A `None` requirement is a no-op. When set, the host
    /// version must be known and satisfy the constraint. Applies to every
    /// OpenClaw adapter (plugin and skill_bundle alike).
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] when the version cannot be determined;
    /// [`AdapterError::InvalidAdapterInput`] when the requirement is malformed
    /// (a manifest bug); [`AdapterError::FrameworkVersionMismatch`] when the
    /// detected version does not satisfy the requirement.
    fn enforce_version_gate(
        &self,
        ctx: &DriverCtx,
        version: Option<&OpenClawVersion>,
        version_display: &str,
    ) -> Result<(), AdapterError> {
        // A missing field (`None`) means "no requirement". OpenClaw owns the
        // validity check for its own adapters (the framework-agnostic Manager
        // does not gate other frameworks on this), so a present-but-empty
        // requirement is a declaration error here, not a silent no-op.
        let Some(raw) = ctx.framework_version_req.as_deref() else {
            return Ok(());
        };
        let req = raw.trim();
        if req.is_empty() {
            return Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: ctx.framework.clone(),
                reason: "adapter framework_version requirement is present but empty".to_string(),
            });
        }
        let version = version.ok_or_else(|| AdapterError::FrameworkCli {
            program: openclaw_bin(),
            reason: format!(
                "cannot determine openclaw version (from `openclaw --version`: {version_display:?}) to check adapter requirement '{req}'"
            ),
        })?;
        match openclaw_version_req_satisfied(req, version) {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdapterError::FrameworkVersionMismatch {
                framework: self.name().to_string(),
                detected: version.to_string(),
                required: req.to_string(),
            }),
            Err(reason) => Err(AdapterError::InvalidAdapterInput {
                component: ctx.component.clone(),
                framework: ctx.framework.clone(),
                reason: format!("invalid adapter framework_version requirement: {reason}"),
            }),
        }
    }

    /// Probe the version and enforce the adapter-level requirement for a
    /// skill_bundle adapter. Only probes when a requirement is declared, so a
    /// requirement-free skill bundle keeps its previous no-CLI behavior.
    ///
    /// # Errors
    ///
    /// As [`Self::enforce_version_gate`].
    fn gate_skill_bundle_version(&self, ctx: &DriverCtx) -> Result<(), AdapterError> {
        // Only probe when a requirement is declared, so a requirement-free
        // skill bundle keeps its previous no-CLI behavior. `enforce_version_gate`
        // still validates a present-but-empty requirement.
        if ctx.framework_version_req.is_none() {
            return Ok(());
        }
        let (version, display) = self.probe_version(ctx)?;
        self.enforce_version_gate(ctx, version.as_ref(), &display)
    }

    /// Resolve the full plugin preflight before any mutation: profile (all
    /// read-only probes), version precondition, `--json` inspect precondition,
    /// version gate, install command, and selected config. Fails closed — the
    /// version must be parseable, the host must expose machine-readable
    /// (`--json`) inspect output for verification, and the `--force` /
    /// unsafe-flag capabilities must hold (see [`build_install_cmd`]).
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] / [`AdapterError::FrameworkVersionMismatch`]
    /// / [`AdapterError::InvalidAdapterInput`] for any failed probe,
    /// precondition, gate, or malformed condition.
    fn plugin_preflight<'a>(
        &self,
        resource_root: &Path,
        ctx: &'a DriverCtx,
    ) -> Result<PluginPreflight<'a>, AdapterError> {
        let home = require_home(ctx)?;
        let profile = self.host_profile(ctx)?;
        // Fail closed before the first write: the version must be known even
        // when the manifest declares no version condition, so an unreadable
        // `--version` never silently proceeds to an install.
        if profile.version.is_none() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "cannot determine openclaw version from `openclaw --version` (output: {:?})",
                    profile.version_display
                ),
            });
        }
        // Runtime verification relies on machine-readable inspect output, so a
        // host without `--json` is rejected before install, not after.
        if !profile.supports_inspect_json {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: "`openclaw plugins inspect --help` does not expose --json; \
                         cannot verify plugin runtime status"
                    .to_string(),
            });
        }
        self.enforce_version_gate(ctx, profile.version.as_ref(), &profile.version_display)?;
        let install_cmd = build_install_cmd(
            resource_root,
            &home,
            ctx.user_home.as_deref(),
            &profile,
            ctx.allow_unsafe_plugin_install,
        )?;
        let selected_config = self.select_config(ctx, &profile)?;
        Ok(PluginPreflight {
            supports_accept_capabilities: profile.supports_accept_capabilities,
            supports_enable_accept_capabilities: profile.supports_enable_accept_capabilities,
            supports_unsafe_install: profile.unsafe_install_support.is_effective(),
            supports_inspect_json: profile.supports_inspect_json,
            supports_inspect_runtime: profile.supports_inspect_runtime,
            selected_config,
            install_cmd,
        })
    }

    /// Select the declared config entries whose optional `framework_version`
    /// condition the host version satisfies, paired with their original
    /// manifest index. A missing condition (`None`) always selects; a
    /// condition the host does not satisfy is skipped (left out of the
    /// receipt). A present-but-empty or malformed condition is a manifest bug.
    ///
    /// # Errors
    ///
    /// [`AdapterError::InvalidAdapterInput`] when a condition is present but
    /// empty or malformed; [`AdapterError::FrameworkCli`] when a valid
    /// condition cannot be evaluated because the host version is unknown.
    fn select_config<'a>(
        &self,
        ctx: &'a DriverCtx,
        profile: &OpenClawHostProfile,
    ) -> Result<Vec<(usize, &'a AdapterConfigSetSpec)>, AdapterError> {
        let mut selected = Vec::new();
        for (i, cfg) in ctx.declared_config.iter().enumerate() {
            // A missing field means "always apply"; an explicit empty value is
            // a declaration error, not an implicit unconditional apply.
            let Some(raw) = cfg.framework_version.as_deref() else {
                selected.push((i, cfg));
                continue;
            };
            let req = raw.trim();
            if req.is_empty() {
                return Err(AdapterError::InvalidAdapterInput {
                    component: ctx.component.clone(),
                    framework: ctx.framework.clone(),
                    reason: format!(
                        "config '{}' declares an empty framework_version condition",
                        cfg.key
                    ),
                });
            }
            let version = profile.version.as_ref().ok_or_else(|| AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "cannot evaluate config version condition '{req}' for key '{}': openclaw version unknown",
                    cfg.key
                ),
            })?;
            match openclaw_version_req_satisfied(req, version) {
                Ok(true) => selected.push((i, cfg)),
                Ok(false) => {}
                Err(reason) => {
                    return Err(AdapterError::InvalidAdapterInput {
                        component: ctx.component.clone(),
                        framework: ctx.framework.clone(),
                        reason: format!(
                            "config '{}' has an invalid framework_version condition: {reason}",
                            cfg.key
                        ),
                    });
                }
            }
        }
        Ok(selected)
    }

    /// Verify the plugin reports `loaded` after install/skill/config apply.
    ///
    /// Uses `plugins inspect <id> --runtime --json` when `with_runtime` (the
    /// host's `--runtime` support, resolved during prepare and passed via
    /// [`PreparedEnable`]), else `plugins inspect <id> --json`. No inspect-help
    /// probe runs here — it already ran during prepare. The JSON is parsed in
    /// Rust (legacy diagnostic lines before it are tolerated) and
    /// `.plugin.status` must equal `"loaded"`.
    ///
    /// # Errors
    ///
    /// [`AdapterError::FrameworkCli`] carrying the OpenClaw diagnostics when
    /// the command fails, the JSON is missing/invalid, or the status is not
    /// `loaded`.
    fn verify_runtime(
        &self,
        plugin_id: &str,
        home: &Path,
        user_home: Option<&Path>,
        ctx: &DriverCtx,
        with_runtime: bool,
    ) -> Result<(), AdapterError> {
        let cmd = build_inspect_cmd(plugin_id, home, user_home, with_runtime);
        let output = ctx.ops.run_framework_cli(cmd)?;
        if !output.success() {
            return Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "runtime verification of plugin '{plugin_id}' failed: {}",
                    inspect_diagnostics(&output)
                ),
            });
        }
        let value =
            extract_trailing_json(&output.stdout).ok_or_else(|| AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "could not parse `plugins inspect` JSON for plugin '{plugin_id}'; diagnostics: {}",
                    inspect_diagnostics(&output)
                ),
            })?;
        let status = value
            .get("plugin")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.as_str());
        match status {
            Some("loaded") => Ok(()),
            other => Err(AdapterError::FrameworkCli {
                program: openclaw_bin(),
                reason: format!(
                    "plugin '{plugin_id}' runtime status is {} (expected \"loaded\"); diagnostics: {}",
                    other
                        .map(|s| format!("\"{s}\""))
                        .unwrap_or_else(|| "absent".to_string()),
                    inspect_diagnostics(&output)
                ),
            }),
        }
    }
}

/// Merge a command's stdout and stderr into one searchable string. Help and
/// version output land on either stream across CLI implementations.
fn combine_output(output: &CliOutput) -> String {
    let mut s = output.stdout.clone();
    if !output.stderr.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&output.stderr);
    }
    s
}

/// Compose a compact diagnostics string from an inspect command's output,
/// preserving the framework's own messages for the operator.
fn inspect_diagnostics(output: &CliOutput) -> String {
    let mut parts = Vec::new();
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        parts.push(stdout.to_string());
    }
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        parts.push(format!("stderr: {stderr}"));
    }
    if output.timed_out {
        parts.push("timed out".to_string());
    }
    if parts.is_empty() {
        "<no output>".to_string()
    } else {
        parts.join("; ")
    }
}

/// Parse a JSON object from `stdout`, tolerating legacy diagnostic lines
/// printed before it. Tries the whole trimmed output first, then each `{`
/// boundary in turn — OpenClaw prints the JSON last, so the first prefix
/// that parses cleanly is the intended value.
fn extract_trailing_json(stdout: &str) -> Option<serde_json::Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    for (idx, _) in stdout.char_indices().filter(|&(_, c)| c == '{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout[idx..].trim()) {
            return Some(value);
        }
    }
    None
}

/// Validate prepared config indices before the first mutation.
fn validate_prepared_config_indices(
    indices: &[usize],
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let mut previous = None;
    for &index in indices {
        if index >= ctx.declared_config.len() {
            return Err(prepared_state_mismatch(&format!(
                "config index {index} is outside the declared config"
            )));
        }
        if previous.is_some_and(|value| value >= index) {
            return Err(prepared_state_mismatch(
                "selected config indices must be unique and in manifest order",
            ));
        }
        previous = Some(index);
    }
    Ok(())
}

/// Validate the OpenClaw payload's applied-resource references and the
/// write-ahead state of every config resource.
fn validate_config_claim_state(claim: &AdapterClaim) -> Result<(), AdapterError> {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };

    let mut applied_ids = HashSet::new();
    for resource_id in &payload.config_resources {
        if !applied_ids.insert(resource_id.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("applied config resource '{resource_id}' is referenced more than once"),
            ));
        }
    }

    let mut resource_ids = HashSet::new();
    let mut config_keys = HashSet::new();
    let mut has_pending = false;
    for resource in &claim.resources {
        if !resource_ids.insert(resource.id.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("resource id '{}' is duplicated", resource.id),
            ));
        }
        let ClaimResourceKind::FrameworkConfig { key, state, .. } = &resource.kind else {
            continue;
        };
        if !config_keys.insert(key.as_str()) {
            return Err(invalid_config_claim(
                claim,
                &format!("config key '{key}' has more than one receipt resource"),
            ));
        }
        let referenced_as_applied = applied_ids.contains(resource.id.as_str());
        match (*state, referenced_as_applied) {
            (ConfigApplyState::Applied, true) | (ConfigApplyState::Pending, false) => {}
            (ConfigApplyState::Applied, false) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "applied config resource '{}' is missing from the OpenClaw payload",
                        resource.id
                    ),
                ));
            }
            (ConfigApplyState::Pending, true) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "pending config resource '{}' is listed as applied",
                        resource.id
                    ),
                ));
            }
        }
        has_pending |= *state == ConfigApplyState::Pending;
    }

    for resource_id in applied_ids {
        match claim.resource(resource_id).map(|resource| &resource.kind) {
            Some(ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Applied,
                ..
            }) => {}
            Some(_) => {
                return Err(invalid_config_claim(
                    claim,
                    &format!(
                        "applied config resource '{resource_id}' does not reference confirmed config"
                    ),
                ));
            }
            None => {
                return Err(invalid_config_claim(
                    claim,
                    &format!("applied config resource '{resource_id}' does not exist"),
                ));
            }
        }
    }
    if has_pending && claim.status != ClaimStatus::CleanupFailed {
        return Err(invalid_config_claim(
            claim,
            "pending config resources require cleanup_failed receipt status",
        ));
    }
    Ok(())
}

/// Ensure every uncertain prior config can be replayed by this enable.
///
/// A removed or version-incompatible key cannot be confirmed or superseded.
/// Reject it before another framework mutation so enable never reports success
/// while retaining an indefinitely pending receipt.
fn validate_pending_config_selection(
    claim: &AdapterClaim,
    selected_indices: &[usize],
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let selected_keys: HashSet<&str> = selected_indices
        .iter()
        .map(|&index| ctx.declared_config[index].key.as_str())
        .collect();
    let unmatched_keys: Vec<&str> = claim
        .resources
        .iter()
        .filter_map(|resource| match &resource.kind {
            ClaimResourceKind::FrameworkConfig {
                key,
                state: ConfigApplyState::Pending,
                ..
            } if !selected_keys.contains(key.as_str()) => Some(key.as_str()),
            _ => None,
        })
        .collect();
    if unmatched_keys.is_empty() {
        return Ok(());
    }

    Err(AdapterError::InvalidAdapterInput {
        component: claim.component.clone(),
        framework: claim.framework.clone(),
        reason: format!(
            "pending OpenClaw config key(s) [{}] are not selected by the current manifest and \
             host version; restore matching config entries and re-run enable to reconcile them, \
             or disable the adapter to acknowledge they are left in place",
            unmatched_keys.join(", ")
        ),
    })
}

/// Carry confirmed and uncertain config facts into a replacement receipt so a
/// re-enable failure cannot erase host state that the new attempt did not
/// supersede.
fn preserve_openclaw_config_facts(
    prior: &AdapterClaim,
    next: &mut AdapterClaim,
) -> Result<(), AdapterError> {
    validate_config_claim_state(prior)?;
    validate_config_claim_state(next)?;

    let prior_resources: Vec<ClaimResource> = prior
        .resources
        .iter()
        .filter(|resource| matches!(resource.kind, ClaimResourceKind::FrameworkConfig { .. }))
        .cloned()
        .collect();
    for resource in prior_resources {
        match next.resource(&resource.id) {
            Some(existing) if existing == &resource => {}
            Some(_) => {
                return Err(invalid_config_claim(
                    next,
                    &format!(
                        "prior config resource id '{}' collides with the replacement receipt",
                        resource.id
                    ),
                ));
            }
            None => next.resources.push(resource),
        }
    }

    let DriverPayload::OpenClaw(prior_payload) = &prior.driver_payload else {
        return Err(invalid_config_claim(
            prior,
            "receipt payload is not OpenClaw",
        ));
    };
    let DriverPayload::OpenClaw(next_payload) = &mut next.driver_payload else {
        return Err(invalid_config_claim(
            next,
            "receipt payload is not OpenClaw",
        ));
    };
    for resource_id in &prior_payload.config_resources {
        if !next_payload.config_resources.contains(resource_id) {
            next_payload.config_resources.push(resource_id.clone());
        }
    }
    if next.resources.iter().any(|resource| {
        matches!(
            resource.kind,
            ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Pending,
                ..
            }
        )
    }) {
        next.status = ClaimStatus::CleanupFailed;
    }
    validate_config_claim_state(next)
}

/// Ensure one selected config key has a durable typed intent resource.
///
/// Existing matching applied or pending resources are reused, which keeps
/// repeated apply idempotent. When a manifest index collides with a preserved
/// resource for another key, a deterministic numeric suffix avoids erasing the
/// prior fact.
fn ensure_config_intent(
    claim: &mut AdapterClaim,
    index: usize,
    config: &AdapterConfigSetSpec,
) -> Result<String, AdapterError> {
    validate_config_claim_state(claim)?;

    let existing_id = claim.resources.iter().find_map(|resource| {
        matches!(
            &resource.kind,
            ClaimResourceKind::FrameworkConfig { framework, key, .. }
                if framework == &claim.framework && key == &config.key
        )
        .then(|| resource.id.clone())
    });

    let resource_id = match existing_id {
        Some(resource_id) => resource_id,
        None => {
            let base_id = format!("openclaw_config_{index}");
            let resource_id = if claim.resource(&base_id).is_none() {
                base_id
            } else {
                let mut suffix = 1usize;
                loop {
                    let candidate = format!("{base_id}_{suffix}");
                    if claim.resource(&candidate).is_none() {
                        break candidate;
                    }
                    suffix += 1;
                }
            };
            let framework = claim.framework.clone();
            claim.resources.push(ClaimResource {
                id: resource_id.clone(),
                purpose: "openclaw_config".to_string(),
                kind: ClaimResourceKind::FrameworkConfig {
                    framework,
                    key: config.key.clone(),
                    state: ConfigApplyState::Pending,
                },
            });
            resource_id
        }
    };

    let Some(resource) = claim
        .resources
        .iter_mut()
        .find(|resource| resource.id == resource_id)
    else {
        return Err(invalid_config_claim(
            claim,
            &format!("config resource '{resource_id}' disappeared before apply"),
        ));
    };
    match &mut resource.kind {
        ClaimResourceKind::FrameworkConfig { state, .. } => {
            *state = ConfigApplyState::Pending;
        }
        _ => {
            return Err(invalid_config_claim(
                claim,
                &format!("config resource '{resource_id}' is not framework config"),
            ));
        }
    }
    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    payload
        .config_resources
        .retain(|existing| existing != &resource_id);
    claim.status = ClaimStatus::CleanupFailed;
    validate_config_claim_state(claim)?;
    Ok(resource_id)
}

/// Promote a matching pending resource to a confirmed applied fact.
fn confirm_config_applied(
    claim: &mut AdapterClaim,
    resource_id: &str,
    config: &AdapterConfigSetSpec,
) -> Result<(), AdapterError> {
    let framework = claim.framework.clone();
    let Some(resource_index) = claim
        .resources
        .iter()
        .position(|resource| resource.id == resource_id)
    else {
        return Err(invalid_config_claim(
            claim,
            &format!("config resource '{resource_id}' disappeared during apply"),
        ));
    };
    let resource = &mut claim.resources[resource_index];
    match &mut resource.kind {
        ClaimResourceKind::FrameworkConfig {
            framework: resource_framework,
            key,
            state,
        } if resource_framework == &framework && key == &config.key => {
            *state = ConfigApplyState::Applied;
        }
        _ => {
            return Err(invalid_config_claim(
                claim,
                &format!(
                    "config resource '{resource_id}' does not match '{}'",
                    config.key
                ),
            ));
        }
    }

    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        return Err(invalid_config_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    if !payload
        .config_resources
        .iter()
        .any(|existing| existing == resource_id)
    {
        payload.config_resources.push(resource_id.to_string());
    }
    claim.status = if claim.resources.iter().any(|resource| {
        matches!(
            resource.kind,
            ClaimResourceKind::FrameworkConfig {
                state: ConfigApplyState::Pending,
                ..
            }
        )
    }) {
        ClaimStatus::CleanupFailed
    } else {
        ClaimStatus::Enabled
    };
    validate_config_claim_state(claim)?;
    Ok(())
}

fn invalid_config_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw config receipt: {reason}"),
    }
}

/// Whether an install command's output looks like an OpenClaw plugin-safety
/// rejection (as opposed to a generic failure). Used only to decide whether
/// to surface the explicit-retry hint — never to auto-retry.
fn install_output_looks_like_safety_rejection(output: &CliOutput) -> bool {
    let haystack = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    ["unsafe", "safety", "dangerous"]
        .iter()
        .any(|marker| haystack.contains(marker))
}

fn install_output_requires_capability_consent(output: &CliOutput) -> bool {
    format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    )
    .to_ascii_lowercase()
    .contains("capability consent")
}

// ---------------------------------------------------------------------------
// OpenClaw version parsing and comparison
// ---------------------------------------------------------------------------

/// A parsed OpenClaw version.
///
/// OpenClaw ships calendar-style versions (`2026.4.14`) that may carry a
/// purely-numeric *correction* suffix (`2026.5.3-1`, a rebuild of the same
/// release), an alphabetic prerelease (`2026.4.14-beta.1`, `-rc.2`), and/or
/// `+build` metadata (ignored for ordering).
///
/// The deliberate departure from plain semver: a numeric-only suffix is a
/// correction that sorts **above** the base version, not a prerelease that
/// sorts below. Comparing with a stock semver `Version` would wrongly rank
/// `2026.5.3-1 < 2026.5.3`, so this type implements its own ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenClawVersion {
    core: [u64; 3],
    suffix: VersionSuffix,
}

/// The suffix of an [`OpenClawVersion`], ranked prerelease < release <
/// correction.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionSuffix {
    /// Alphabetic prerelease identifiers (`beta.1`); sorts below the release.
    Prerelease(Vec<PreId>),
    /// No suffix.
    Release,
    /// Numeric-only correction identifiers (`1`, `2.1`); sorts above release.
    Correction(Vec<u64>),
}

/// One prerelease identifier, numeric or textual, compared semver-style.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreId {
    /// Purely-numeric identifier, compared by value.
    Num(u64),
    /// Textual identifier, compared lexically.
    Text(String),
}

impl OpenClawVersion {
    /// Parse an OpenClaw version string. Returns `None` for any malformed
    /// input — a non-numeric or >3-component core, an empty or invalid
    /// `-suffix`, or empty/invalid `+build` metadata — so a bad constraint
    /// like `>=2026.4.14-` is rejected rather than silently treated as
    /// `>=2026.4.14`. Build metadata is validated for well-formedness, then
    /// discarded (it does not affect identity or ordering).
    fn parse(input: &str) -> Option<OpenClawVersion> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        // Separate optional `+build`; validate then drop it. An empty or
        // invalid build metadata segment is a malformed version.
        let s = match s.split_once('+') {
            Some((before, build)) => {
                if !is_valid_dot_identifiers(build) {
                    return None;
                }
                before
            }
            None => s,
        };
        // Separate optional `-suffix`. A trailing `-` with no suffix is
        // malformed, not a plain release.
        let (core_str, pre_str) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };

        let mut core = [0u64; 3];
        let mut count = 0usize;
        for (i, part) in core_str.split('.').enumerate() {
            if i >= 3 {
                return None;
            }
            core[i] = part.parse().ok()?;
            count = i + 1;
        }
        if count == 0 {
            return None;
        }

        let suffix = match pre_str {
            None => VersionSuffix::Release,
            Some(pre) => {
                // Every identifier must be non-empty and drawn from the
                // semver alphabet (`[0-9A-Za-z-]`); an empty `-` or an illegal
                // character is rejected rather than accepted as text.
                if !is_valid_dot_identifiers(pre) {
                    return None;
                }
                let ids: Vec<&str> = pre.split('.').collect();
                let all_numeric = ids.iter().all(|id| id.bytes().all(|b| b.is_ascii_digit()));
                if all_numeric {
                    let nums = ids
                        .iter()
                        .map(|id| id.parse::<u64>().ok())
                        .collect::<Option<Vec<_>>>()?;
                    VersionSuffix::Correction(nums)
                } else {
                    let parsed = ids
                        .iter()
                        .map(|id| {
                            if id.bytes().all(|b| b.is_ascii_digit()) {
                                id.parse::<u64>().ok().map(PreId::Num)
                            } else {
                                Some(PreId::Text((*id).to_string()))
                            }
                        })
                        .collect::<Option<Vec<_>>>()?;
                    VersionSuffix::Prerelease(parsed)
                }
            }
        };
        Some(OpenClawVersion { core, suffix })
    }
}

/// Whether `s` is one or more dot-separated identifiers, each non-empty and
/// composed only of ASCII alphanumerics and `-` (the semver
/// prerelease/build alphabet). Rejects empty input and empty identifiers
/// (leading/trailing/double dots).
fn is_valid_dot_identifiers(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split('.')
        .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

impl std::fmt::Display for OpenClawVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.core[0], self.core[1], self.core[2])?;
        match &self.suffix {
            VersionSuffix::Release => Ok(()),
            VersionSuffix::Correction(nums) => {
                let joined = nums
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(".");
                write!(f, "-{joined}")
            }
            VersionSuffix::Prerelease(ids) => {
                let joined = ids
                    .iter()
                    .map(|id| match id {
                        PreId::Num(n) => n.to_string(),
                        PreId::Text(t) => t.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                write!(f, "-{joined}")
            }
        }
    }
}

impl Ord for OpenClawVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| self.suffix.cmp(&other.suffix))
    }
}

impl PartialOrd for OpenClawVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl VersionSuffix {
    /// Ordering rank: prerelease (0) < release (1) < correction (2).
    fn rank(&self) -> u8 {
        match self {
            VersionSuffix::Prerelease(_) => 0,
            VersionSuffix::Release => 1,
            VersionSuffix::Correction(_) => 2,
        }
    }
}

impl Ord for VersionSuffix {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (VersionSuffix::Prerelease(a), VersionSuffix::Prerelease(b)) => cmp_pre_ids(a, b),
            (VersionSuffix::Correction(a), VersionSuffix::Correction(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for VersionSuffix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare prerelease identifier lists semver-style: numeric identifiers
/// compare by value, numeric sorts below textual, textual compares lexically,
/// and a shorter list sorts below a longer one when the shared prefix is
/// equal.
fn cmp_pre_ids(a: &[PreId], b: &[PreId]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x, y) {
            (PreId::Num(m), PreId::Num(n)) => m.cmp(n),
            (PreId::Text(m), PreId::Text(n)) => m.cmp(n),
            (PreId::Num(_), PreId::Text(_)) => Ordering::Less,
            (PreId::Text(_), PreId::Num(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// Comparison operators supported in an OpenClaw version requirement.
#[derive(Debug, Clone, Copy)]
enum ReqOp {
    /// `>=`
    Gte,
    /// `>`
    Gt,
    /// `<=`
    Lte,
    /// `<`
    Lt,
    /// `=` / `==`
    Eq,
}

/// Split a single requirement clause into its operator and version text. A
/// bare version (no operator) is treated as a minimum (`>=`), matching how
/// these fields express a lowest supported framework version.
fn split_req_clause(clause: &str) -> (ReqOp, &str) {
    const OPS: [(&str, ReqOp); 6] = [
        (">=", ReqOp::Gte),
        ("<=", ReqOp::Lte),
        ("==", ReqOp::Eq),
        (">", ReqOp::Gt),
        ("<", ReqOp::Lt),
        ("=", ReqOp::Eq),
    ];
    for (prefix, op) in OPS {
        if let Some(rest) = clause.strip_prefix(prefix) {
            return (op, rest.trim());
        }
    }
    (ReqOp::Gte, clause)
}

/// Evaluate a comma-separated requirement (`>=2026.4.14, <2027.0.0`) against
/// a version. All clauses must hold.
///
/// # Errors
///
/// Returns `Err(reason)` when the requirement is empty, contains an empty
/// clause (a leading/trailing/double comma), or a clause's version cannot be
/// parsed — all manifest bugs, distinct from a non-match.
fn openclaw_version_req_satisfied(req: &str, version: &OpenClawVersion) -> Result<bool, String> {
    if req.trim().is_empty() {
        return Err(format!("empty version requirement '{req}'"));
    }
    let mut clauses = Vec::new();
    for clause in req.split(',') {
        let clause = clause.trim();
        // An empty clause (from `>=X,,<Y` or a trailing/leading comma) is a
        // malformed requirement, not something to silently skip.
        if clause.is_empty() {
            return Err(format!("empty clause in version requirement '{req}'"));
        }
        let (op, ver_str) = split_req_clause(clause);
        let required = OpenClawVersion::parse(ver_str)
            .ok_or_else(|| format!("unparseable version '{ver_str}' in requirement '{req}'"))?;
        clauses.push((op, required));
    }

    Ok(clauses.into_iter().all(|(op, required)| {
        let ord = version.cmp(&required);
        match op {
            ReqOp::Gte => ord != Ordering::Less,
            ReqOp::Gt => ord == Ordering::Greater,
            ReqOp::Lte => ord != Ordering::Greater,
            ReqOp::Lt => ord == Ordering::Less,
            ReqOp::Eq => ord == Ordering::Equal,
        }
    }))
}

/// Extract an [`OpenClawVersion`] from `openclaw --version` output.
///
/// Only an unambiguous version line is accepted: either a bare calendar-shaped
/// version or a version following an explicit `OpenClaw`, `OpenClaw version`,
/// or `OpenClaw CLI version` label. Calendar-shaped tokens embedded in warning
/// or diagnostic lines are ignored. Zero or multiple candidates return `None`
/// so callers fail closed instead of guessing which number belongs to the CLI.
fn parse_openclaw_version_output(output: &str) -> Option<OpenClawVersion> {
    let candidates: Vec<OpenClawVersion> = output
        .lines()
        .filter_map(parse_openclaw_version_line)
        .collect();
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

fn parse_openclaw_version_line(line: &str) -> Option<OpenClawVersion> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (version_token, trailing) = match tokens.as_slice() {
        [version] => (*version, &[][..]),
        [label, cli, keyword, version, trailing @ ..]
            if is_openclaw_label(label)
                && cli.eq_ignore_ascii_case("cli")
                && keyword.eq_ignore_ascii_case("version") =>
        {
            (*version, trailing)
        }
        [label, keyword, version, trailing @ ..]
            if is_openclaw_label(label) && keyword.eq_ignore_ascii_case("version") =>
        {
            (*version, trailing)
        }
        [label, version, trailing @ ..] if is_openclaw_label(label) => (*version, trailing),
        _ => return None,
    };
    if !trailing_version_annotation_is_known(trailing) {
        return None;
    }
    let version_token = version_token
        .strip_prefix('v')
        .or_else(|| version_token.strip_prefix('V'))
        .unwrap_or(version_token);
    is_calendar_shaped_token(version_token)
        .then(|| OpenClawVersion::parse(version_token))
        .flatten()
}

fn is_openclaw_label(token: &str) -> bool {
    token.trim_end_matches(':').eq_ignore_ascii_case("openclaw")
}

fn trailing_version_annotation_is_known(tokens: &[&str]) -> bool {
    tokens.is_empty()
        || (tokens.first().is_some_and(|token| token.starts_with('('))
            && tokens.last().is_some_and(|token| token.ends_with(')')))
}

/// Whether `token` has an OpenClaw calendar-shaped core: exactly three
/// dot-separated numeric components with a 4-digit leading year, ignoring any
/// `-`/`+` suffix. `2026.4.14` and `2026.5.3-1` pass; `22.14.0` and `2026.4`
/// (a short version, valid only in a *requirement*) do not.
fn is_calendar_shaped_token(token: &str) -> bool {
    let core = token.split(['-', '+']).next().unwrap_or(token);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let year_ok = parts[0].len() == 4 && parts[0].bytes().all(|b| b.is_ascii_digit());
    year_ok
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------------------
// Pure helpers (no spawning) — unit-testable
// ---------------------------------------------------------------------------

/// `OPENCLAW_BIN` override, else `openclaw`.
fn openclaw_bin() -> String {
    std::env::var("OPENCLAW_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "openclaw".to_string())
}

/// Resolve the OpenClaw state directory using the runtime's whitespace,
/// tilde-expansion, and absolute-path semantics. `OPENCLAW_HOME` remains a
/// direct state-directory fallback for compatibility with earlier releases.
fn openclaw_home(user_home: Option<&Path>) -> Option<PathBuf> {
    let home_override = std::env::var_os("OPENCLAW_HOME")
        .and_then(|value| resolve_openclaw_path(&value, user_home));
    let tilde_home = home_override.as_deref().or(user_home);

    if let Some(state_dir) = std::env::var_os("OPENCLAW_STATE_DIR")
        && let Some(state_dir) = resolve_openclaw_path(&state_dir, tilde_home)
    {
        return Some(state_dir);
    }

    home_override
        .or_else(|| user_home.and_then(|home| absolute_normalized_path(home.join(".openclaw"))))
}

/// External roots accepted for current receipts plus the exact root the
/// pre-fix resolver could have persisted. The compatibility root is rebuilt
/// from trusted process/user context, never from receipt contents.
fn openclaw_allowed_roots(user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(current) = openclaw_home(user_home) {
        roots.push(current);
    }
    if let Some(legacy) = legacy_openclaw_home(user_home)
        && !roots.contains(&legacy)
    {
        roots.push(legacy);
    }
    roots
}

/// Reproduce the old resolver only to validate receipts it created. New CLI
/// operations and receipts always use [`openclaw_home`].
fn legacy_openclaw_home(user_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("OPENCLAW_HOME") {
        let home = home.to_string_lossy();
        let trimmed = home.trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    user_home.map(|h| h.join(".openclaw"))
}

/// Match OpenClaw's `resolveUserPath`: trim, expand a leading `~`, resolve
/// relative paths from the current directory, and normalize lexically.
fn resolve_openclaw_path(value: &OsStr, tilde_home: Option<&Path>) -> Option<PathBuf> {
    let value = value.to_string_lossy();
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let path = if value == "~" {
        tilde_home
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())?
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        tilde_home
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())?
            .join(rest)
    } else {
        PathBuf::from(value)
    };
    absolute_normalized_path(path)
}

fn absolute_normalized_path(path: PathBuf) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Some(normalize_lexically(&absolute))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// PATH prefix dirs, mirroring `install.sh`:
/// `<user_home>/.local/bin`, `<home>/bin`, `/usr/local/bin`.
fn path_prepend(home: &Path, user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(uh) = user_home {
        v.push(uh.join(".local/bin"));
    }
    v.push(home.join("bin"));
    v.push(PathBuf::from("/usr/local/bin"));
    v
}

/// Keep every OpenClaw invocation on the resolved state directory while
/// suppressing the legacy home override.
fn base_cmd(args: Vec<String>, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    FrameworkCommand {
        program: openclaw_bin(),
        args,
        stdin: None,
        env_set: vec![(
            "OPENCLAW_STATE_DIR".to_string(),
            home.to_string_lossy().into_owned(),
        )],
        env_remove: vec!["OPENCLAW_HOME".to_string()],
        path_prepend: path_prepend(home, user_home),
        timeout: CLI_TIMEOUT,
    }
}

/// Build a host-compatible `openclaw plugins install` command.
///
/// `--force` is a required capability of the driver contract; if the host's
/// install help does not expose it, this fails before any mutation. The
/// unsafe flag is appended only when the caller both authorized it
/// (`allow_unsafe`) and the host's help describes it as effective. An
/// authorized request fails when the option is absent or advertised as a
/// deprecated no-op, and a normal install never carries it.
/// Enabling a plugin accepts its declared capabilities, so the consent flag
/// is included whenever the installer advertises it.
///
/// # Errors
///
/// [`AdapterError::FrameworkCli`] when `--force` is unsupported, or when an
/// authorized unsafe install is requested but the host does not expose an
/// effective unsafe flag.
fn build_install_cmd(
    resource_root: &Path,
    home: &Path,
    user_home: Option<&Path>,
    profile: &OpenClawHostProfile,
    allow_unsafe: bool,
) -> Result<FrameworkCommand, AdapterError> {
    if !profile.supports_install_force {
        return Err(AdapterError::FrameworkCli {
            program: openclaw_bin(),
            reason: "`openclaw plugins install --help` does not expose the required \
                     --force flag; cannot install non-interactively"
                .to_string(),
        });
    }
    if allow_unsafe {
        match profile.unsafe_install_support {
            UnsafeInstallSupport::Effective => {}
            UnsafeInstallSupport::Unsupported => {
                return Err(AdapterError::FrameworkCli {
                    program: openclaw_bin(),
                    reason: "unsafe plugin install was explicitly authorized but this openclaw \
                             does not expose --dangerously-force-unsafe-install"
                        .to_string(),
                });
            }
            UnsafeInstallSupport::DeprecatedNoOp => {
                return Err(AdapterError::FrameworkCli {
                    program: openclaw_bin(),
                    reason: "unsafe plugin install was explicitly authorized, but this openclaw \
                             advertises --dangerously-force-unsafe-install as a deprecated no-op; \
                             configure the operator-owned security.installPolicy instead"
                        .to_string(),
                });
            }
        }
    }
    Ok(base_cmd(
        install_argv(
            resource_root,
            allow_unsafe,
            profile.supports_accept_capabilities,
        ),
        home,
        user_home,
    ))
}

/// Build the `plugins install <root> --force [--dangerously-force-unsafe-install]`
/// argv, accepting declared capabilities when the host supports consent.
/// `--force` is always present (a required capability); the unsafe flag
/// is appended iff `allow_unsafe`. Capability support is the caller's concern
/// ([`build_install_cmd`] verifies it during preflight); `apply_enable` builds
/// this directly from the authorized decision without re-probing.
fn install_argv(
    resource_root: &Path,
    allow_unsafe: bool,
    accept_capabilities: bool,
) -> Vec<String> {
    let mut args = vec![
        "plugins".to_string(),
        "install".to_string(),
        resource_root.to_string_lossy().into_owned(),
        "--force".to_string(),
    ];
    if allow_unsafe {
        args.push("--dangerously-force-unsafe-install".to_string());
    }
    if accept_capabilities {
        args.push("--accept-capabilities".to_string());
    }
    args
}

/// Build explicit activation, or its read-only `--help` probe.
fn build_enable_cmd(
    plugin_id: &str,
    home: &Path,
    user_home: Option<&Path>,
    accept_capabilities: bool,
) -> FrameworkCommand {
    let mut args = vec![
        "plugins".to_string(),
        "enable".to_string(),
        plugin_id.to_string(),
    ];
    if accept_capabilities {
        args.push("--accept-capabilities".to_string());
    }
    base_cmd(args, home, user_home)
}

/// Whether `help` lists `flag` as a standalone option token — not merely as a
/// prefix of a longer flag. A flag token continues through any character that
/// can appear inside an option name (ASCII alphanumeric, `-`, `_`, `.`), so a
/// near-miss like `--force-color`, `--json-file`, `--json_file`, or
/// `--runtime.mode` stays a single token and never matches `--force`/`--json`/
/// `--runtime`, while genuine boundaries (`--json`, `--json=<path>`,
/// `--json,`) do.
fn help_lists_flag(help: &str, flag: &str) -> bool {
    help.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .any(|token| token == flag)
}

/// Classify the unsafe option by its advertised help semantics. Current
/// OpenClaw releases retain the token as a deprecated no-op, so token presence
/// alone must not authorize an ineffective bypass or produce invalid retry
/// advice.
fn unsafe_install_support(help: &str) -> UnsafeInstallSupport {
    const FLAG: &str = "--dangerously-force-unsafe-install";
    let Some(option_line) = help.lines().find(|line| help_lists_flag(line, FLAG)) else {
        return UnsafeInstallSupport::Unsupported;
    };
    let description = option_line.to_ascii_lowercase();
    if description.contains("no-op") || description.contains("no op") {
        UnsafeInstallSupport::DeprecatedNoOp
    } else {
        UnsafeInstallSupport::Effective
    }
}

/// Build the read-only `openclaw --version` probe.
fn build_version_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(vec!["--version".to_string()], home, user_home)
}

/// Build the read-only `openclaw plugins install --help` probe.
fn build_install_help_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "install".to_string(),
            "--help".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build the read-only `openclaw plugins inspect --help` probe.
fn build_inspect_help_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "inspect".to_string(),
            "--help".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build `openclaw plugins inspect <plugin_id> [--runtime] --json` for
/// post-install runtime verification. `--runtime` is included only when the
/// host's inspect help exposes it.
fn build_inspect_cmd(
    plugin_id: &str,
    home: &Path,
    user_home: Option<&Path>,
    with_runtime: bool,
) -> FrameworkCommand {
    let mut args = vec![
        "plugins".to_string(),
        "inspect".to_string(),
        plugin_id.to_string(),
    ];
    if with_runtime {
        args.push("--runtime".to_string());
    }
    args.push("--json".to_string());
    base_cmd(args, home, user_home)
}

/// Build `openclaw plugins uninstall <plugin_id> --force`.
///
/// `--force` skips OpenClaw's interactive confirmation — ANOLISA drives
/// the CLI non-interactively. `plugin_id` is validated by the caller.
fn build_uninstall_cmd(plugin_id: &str, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "uninstall".to_string(),
            plugin_id.to_string(),
            "--force".to_string(),
        ],
        home,
        user_home,
    )
}

/// Build the read-only `openclaw plugins list`.
fn build_list_cmd(home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec!["plugins".to_string(), "list".to_string()],
        home,
        user_home,
    )
}

/// Plugin id declared by the OpenClaw-native plugin manifest, when present.
fn read_plugin_manifest_id(root: &Path, filename: &str) -> Result<Option<String>, AdapterError> {
    #[derive(serde::Deserialize)]
    struct PluginManifest {
        id: Option<String>,
    }

    let path = root.join(filename);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(AdapterError::Io { path, source }),
    };
    let manifest: PluginManifest =
        serde_json::from_slice(&bytes).map_err(|source| AdapterError::BundleInvalid {
            root: root.to_path_buf(),
            reason: format!(
                "failed to parse {} as OpenClaw plugin manifest: {source}",
                path.display()
            ),
        })?;
    let id =
        manifest
            .id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AdapterError::BundleInvalid {
                root: root.to_path_buf(),
                reason: format!("{} does not declare a non-empty id", path.display()),
            })?;
    Ok(Some(id))
}

/// Human-readable form of a command for dry-run/preview output. Display
/// only — never parsed back into an argv.
fn display_command(cmd: &FrameworkCommand) -> String {
    let mut s = String::new();
    for (k, v) in &cmd.env_set {
        s.push_str(&format!("{k}={v} "));
    }
    s.push_str(&cmd.program);
    for a in &cmd.args {
        s.push(' ');
        s.push_str(a);
    }
    s
}

/// Whether a *readable* inventory positively omits this plugin id.
///
/// One definition for all three callers — the enable-side probe, the status-side
/// [`DisplacementBlock`], and the re-enable plan's carry-over branch — because they
/// must agree on what "this host does not have it" means. An unreadable inventory is
/// **not** an omission: only a list the host actually returned and that does not name
/// the id is evidence, which is the same rule the enable-side claim gate applies.
///
/// The plan needs it for a specific reason. A carried-over displacement skips the
/// probe, since by re-enable time the host reads `false` for a plugin *this adapter*
/// disabled and probing would plan the opposite of what the lifecycle does. Skipping
/// the probe must not also skip the existence check: `prepare_enable` probes
/// unconditionally and fails the real enable with
/// [`missing_displacement_target`], so a plan that promised a carry-over for a plugin
/// a framework upgrade has since removed would describe an enable that cannot run.
fn inventory_omits_plugin(inventory: Option<&str>, plugin_id: &str) -> bool {
    inventory.is_some_and(|inventory| !list_contains_plugin(inventory, plugin_id))
}

/// True when `plugin_id` appears in the `plugins list` output.
///
/// Handles three output shapes:
/// 1. Plain text — each line has whitespace-delimited tokens.
/// 2. Rich table without wrapping — tokens appear between │ delimiters.
/// 3. Rich table with wrapping — a cell value is split across
///    consecutive physical lines within the same column.
///
/// ANSI escape codes are stripped before any matching.
fn list_contains_plugin(stdout: &str, plugin_id: &str) -> bool {
    let stripped = strip_ansi(stdout);

    // Fast path: exact whitespace-token match on lines that are NOT
    // table data lines. Table data lines (containing │/┃/║) must go
    // through the table parser, because a wrapped cell fragment can
    // look like a complete token on a single physical line.
    if stripped.lines().any(|line| {
        !line.contains(|c: char| is_cell_delimiter(c))
            && line.split_whitespace().any(|tok| tok == plugin_id)
    }) {
        return true;
    }

    // Table-aware path: parse rows, concatenate wrapped cell text per
    // column, then search each concatenated cell.
    table_contains_token(&stripped, plugin_id)
}

/// Strip ANSI escape sequences (CSI and OSC) from `s`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: consume until a final byte (0x40..=0x7E).
                    for c in chars.by_ref() {
                        if matches!(c, '\x40'..='\x7e') {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: consume until BEL or ST.
                    for c in chars.by_ref() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn is_cell_delimiter(c: char) -> bool {
    matches!(c, '│' | '┃' | '║')
}

fn is_border_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|c| {
            is_cell_delimiter(c)
                || matches!(
                    c,
                    '─' | '━'
                        | '═'
                        | '┌'
                        | '┐'
                        | '└'
                        | '┘'
                        | '├'
                        | '┤'
                        | '┬'
                        | '┴'
                        | '┼'
                        | '┏'
                        | '┓'
                        | '┗'
                        | '┛'
                        | '┣'
                        | '┫'
                        | '┳'
                        | '┻'
                        | '╋'
                        | '┡'
                        | '┩'
                        | '╇'
                        | '╔'
                        | '╗'
                        | '╚'
                        | '╝'
                        | '╠'
                        | '╣'
                        | '╦'
                        | '╩'
                        | '╬'
                        | ' '
                )
        })
}

/// Extract cell text from a line delimited by │/┃/║. Returns `None`
/// when the line has no cell delimiters (not a table data line).
fn extract_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains(|c: char| is_cell_delimiter(c)) {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(|c: char| is_cell_delimiter(c)).collect();
    if parts.len() < 3 {
        return None;
    }
    // Skip the empty segments before the first and after the last │.
    let cells: Vec<String> = parts[1..parts.len() - 1]
        .iter()
        .map(|cell| cell.trim().to_string())
        .collect();
    Some(cells)
}

/// Parse rich-table output into logical rows (merging physical
/// continuation lines), then check whether any cell in any row
/// matches `token` as a whitespace-delimited word.
///
/// A continuation line is detected by the *last* column being empty.
/// In `plugins list` tables the last column is typically Status
/// (`enabled`/`disabled`), which is always populated on the first
/// physical line of a row but empty on continuation lines. This
/// correctly handles the case where *both* Name and ID wrap.
fn table_contains_token(text: &str, token: &str) -> bool {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current: Option<Vec<String>> = None;

    for line in text.lines() {
        if is_border_line(line) {
            if let Some(cells) = current.take() {
                rows.push(cells);
            }
            continue;
        }

        if let Some(cells) = extract_cells(line) {
            let is_continuation = current.is_some() && cells.last().is_some_and(|c| c.is_empty());

            if is_continuation {
                if let Some(cur) = current.as_mut() {
                    for (i, cell) in cells.into_iter().enumerate() {
                        if i < cur.len() && !cell.is_empty() {
                            cur[i].push_str(&cell);
                        }
                    }
                }
            } else {
                if let Some(prev) = current.take() {
                    rows.push(prev);
                }
                current = Some(cells);
            }
        }
    }
    if let Some(cells) = current {
        rows.push(cells);
    }

    rows.iter().any(|row| {
        row.iter().any(|cell| {
            let trimmed = cell.trim();
            trimmed == token || trimmed.split_whitespace().any(|t| t == token)
        })
    })
}

/// This adapter's own framework plugin id, resolved through the payload's
/// `plugin_resource` reference and cross-checked against what it points at.
///
/// "The first [`ClaimResourceKind::FrameworkPlugin`] in `resources`" — what this
/// used to answer — stopped being a safe definition once a receipt could
/// legitimately carry two of them, because a displaced plugin is one too.
/// Reordering two resources by hand then leaves the generic claim validation and
/// the displacement validation both passing, while `disable` uninstalls the
/// *displaced* plugin and keeps this adapter's own, and `status` and the
/// migration preview verify the wrong registration. Following the payload
/// reference instead makes the answer independent of resource order, and
/// checking purpose, framework and the top-level id turns a receipt that has
/// been edited into something that cannot point at the wrong plugin at all.
///
/// `None` means a skill-bundle receipt, which has no plugin of its own.
///
/// # Errors
///
/// [`AdapterError::BundleInvalid`] when the payload is not OpenClaw's, the
/// reference does not resolve, or any of the three consistency checks fails.
fn claim_own_plugin(claim: &AdapterClaim) -> Result<Option<String>, AdapterError> {
    let invalid = |reason: String| AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw plugin receipt: {reason}"),
    };
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Err(invalid("receipt payload is not OpenClaw".to_string()));
    };
    if payload.plugin_resource.is_empty() {
        // A skill bundle owns no plugin. The top-level convenience id must agree,
        // or the receipt claims a plugin it has no resource for.
        return match &claim.plugin_id {
            None => Ok(None),
            Some(plugin_id) => Err(invalid(format!(
                "receipt records plugin id '{plugin_id}' but no plugin resource"
            ))),
        };
    }
    let resource = claim.resource(&payload.plugin_resource).ok_or_else(|| {
        invalid(format!(
            "plugin resource '{}' is missing",
            payload.plugin_resource
        ))
    })?;
    if resource.purpose != PURPOSE_PLUGIN {
        return Err(invalid(format!(
            "plugin resource '{}' has purpose '{}', not '{PURPOSE_PLUGIN}'",
            payload.plugin_resource, resource.purpose
        )));
    }
    let ClaimResourceKind::FrameworkPlugin {
        framework,
        plugin_id,
    } = &resource.kind
    else {
        return Err(invalid(format!(
            "plugin resource '{}' is not a framework plugin resource",
            payload.plugin_resource
        )));
    };
    if framework != &claim.framework {
        return Err(invalid(format!(
            "plugin resource '{}' belongs to framework '{framework}', not '{}'",
            payload.plugin_resource, claim.framework
        )));
    }
    if claim
        .plugin_id
        .as_deref()
        .is_some_and(|top| top != plugin_id)
    {
        return Err(invalid(format!(
            "receipt's top-level plugin id '{}' disagrees with plugin resource '{}' ('{plugin_id}')",
            claim.plugin_id.clone().unwrap_or_default(),
            payload.plugin_resource
        )));
    }
    Ok(Some(plugin_id.clone()))
}

/// Plugin id from a bundle, or [`AdapterError::BundleInvalid`] when none is
/// resolvable.
fn require_plugin_id(bundle: &AdapterBundle) -> Result<String, AdapterError> {
    bundle
        .plugin_id
        .clone()
        .ok_or_else(|| AdapterError::BundleInvalid {
            root: bundle.resource_root.clone(),
            reason: "no plugin id declared in manifest and none discoverable".to_string(),
        })
}

/// OpenClaw state directory, or [`AdapterError::FrameworkCli`] when no
/// explicit state/home override or user home is available.
fn require_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    openclaw_home(ctx.user_home.as_deref()).ok_or_else(|| AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: "cannot resolve OpenClaw state directory (no OPENCLAW_STATE_DIR, OPENCLAW_HOME, or $HOME)"
            .to_string(),
    })
}

/// Fail-closed error for a `PreparedEnable` that does not match the adapter
/// being applied. Signals a driver-contract misuse (a caller handed the wrong
/// prepared state); raised before any mutation.
fn prepared_state_mismatch(reason: &str) -> AdapterError {
    AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: format!("prepared enable state does not match the adapter: {reason}"),
    }
}

/// Compose a failure reason string from a non-success [`CliOutput`].
fn cli_failure_reason(verb: &str, output: &super::driver::CliOutput) -> String {
    if output.timed_out {
        return format!("'{verb}' timed out");
    }
    let code = output
        .status
        .map(|c| c.to_string())
        .unwrap_or_else(|| "killed".to_string());
    let mut reason = format!("'{verb}' exited with {code}");
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        reason.push_str(": ");
        reason.push_str(stderr);
    }
    reason
}

/// Compose a failure reason keeping the exit/timeout status **and both**
/// stdout and stderr.
///
/// Used on every enable-path command failure (probes, install, config set):
/// OpenClaw may report plugin-safety findings on stdout, and a timeout must
/// not drop stderr — the plain [`cli_failure_reason`] omits both. Both streams
/// are already bounded by the Manager's capture cap.
fn full_failure_reason(verb: &str, output: &super::driver::CliOutput) -> String {
    let mut reason = if output.timed_out {
        format!("'{verb}' timed out")
    } else {
        let code = output
            .status
            .map(|c| c.to_string())
            .unwrap_or_else(|| "killed".to_string());
        format!("'{verb}' exited with {code}")
    };
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        reason.push_str("; stderr: ");
        reason.push_str(stderr);
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        reason.push_str("; stdout: ");
        reason.push_str(stdout);
    }
    reason
}

/// Map a bool to a [`ConditionStatus`] (`true`→`True`, `false`→`False`).
fn bool_status(b: bool) -> ConditionStatus {
    if b {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    }
}

/// Roll the framework-detect and plugin-registration signals into a
/// summary, honoring a `cleanup_failed` receipt.
fn summarize(
    claim_status: ClaimStatus,
    framework_detected: bool,
    plugin_registered: ConditionStatus,
    displaced_released: Option<ConditionStatus>,
) -> AdapterSummary {
    if claim_status == ClaimStatus::CleanupFailed {
        return AdapterSummary::CleanupFailed;
    }
    if !framework_detected {
        return AdapterSummary::Degraded;
    }
    // A re-enabled displaced plugin degrades the summary on its own: the
    // adapter's plugin can be perfectly registered and loaded and still have
    // lost every tool name that made it worth enabling.
    if plugin_registered == ConditionStatus::False
        || displaced_released == Some(ConditionStatus::False)
    {
        return AdapterSummary::Degraded;
    }
    if plugin_registered == ConditionStatus::Unknown
        || displaced_released == Some(ConditionStatus::Unknown)
    {
        return AdapterSummary::Unknown;
    }
    AdapterSummary::Healthy
}

/// Build `openclaw config set <key> <value>`.
fn build_config_set_cmd(
    key: &str,
    value: &toml::Value,
    home: &Path,
    user_home: Option<&Path>,
) -> FrameworkCommand {
    base_cmd(
        vec![
            "config".to_string(),
            "set".to_string(),
            key.to_string(),
            config_value_to_cli_string(value),
        ],
        home,
        user_home,
    )
}

/// Convert a TOML value to a string suitable for the `openclaw config set`
/// CLI argument. Strings are passed bare (no quotes); other types use the
/// TOML display representation.
fn config_value_to_cli_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Human-readable display of a config value for plan output.
fn config_value_display(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

/// Resolve the state directory from a manager-validated OpenClaw receipt.
fn claim_state_dir(claim: &AdapterClaim) -> Result<PathBuf, AdapterError> {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Err(invalid_state_dir_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    let resource = claim.resource(&payload.state_dir_resource).ok_or_else(|| {
        invalid_state_dir_claim(
            claim,
            &format!(
                "state directory resource '{}' is missing",
                payload.state_dir_resource
            ),
        )
    })?;
    match &resource.kind {
        ClaimResourceKind::ExternalPath { path } => Ok(path.clone()),
        _ => Err(invalid_state_dir_claim(
            claim,
            &format!(
                "state directory resource '{}' is not an external path",
                payload.state_dir_resource
            ),
        )),
    }
}

fn invalid_state_dir_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw state directory receipt: {reason}"),
    }
}

/// True only for OpenClaw's exact idempotent-uninstall failure. Other
/// non-zero exits must keep the receipt so cleanup can be retried.
fn uninstall_reports_missing_plugin(output: &CliOutput, plugin_id: &str) -> bool {
    if output.timed_out {
        return false;
    }
    let expected = format!("plugin not found: {}", plugin_id.to_ascii_lowercase());
    let combined = format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    );
    let mut lines = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    matches!(lines.next(), Some(line) if line == expected) && lines.next().is_none()
}

fn uninstall_reports_untracked_plugin(output: &CliOutput, plugin_id: &str) -> bool {
    if output.timed_out {
        return false;
    }
    let expected = format!(
        "plugin \"{}\" is not associated with a tracked package install.",
        plugin_id.to_ascii_lowercase()
    );
    let combined = format!(
        "{}\n{}",
        strip_ansi(&output.stdout),
        strip_ansi(&output.stderr)
    );
    let mut lines = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_ascii_lowercase);
    matches!(lines.next(), Some(line) if line == expected || line.starts_with(&format!("{expected} ")))
        && lines.next().is_none()
}

fn json_list_confirms_plugin_absent(output: &CliOutput, plugin_id: &str) -> bool {
    if !output.success() || !output.stderr.trim().is_empty() {
        return false;
    }
    // A text/table miss or a partial discovery report cannot authorize
    // deleting cleanup ownership. Require JSON IDs and clean diagnostics.
    let Ok(report) = serde_json::from_str::<serde_json::Value>(&output.stdout) else {
        return false;
    };
    let Some(plugins) = report.get("plugins").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let mut diagnostics = vec![report.get("diagnostics")];
    if let Some(registry) = report.get("registry") {
        diagnostics.push(registry.get("diagnostics"));
    }
    plugins.iter().all(|plugin| {
        plugin
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty() && id != plugin_id)
    }) && diagnostics.into_iter().all(|value| {
        value
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().all(|item| item["level"] == "info"))
    })
}

/// One displaced framework plugin resolved from a receipt.
struct DisplacedPlugin {
    /// Resource id the receipt's reference actually names — [`DisplacedPluginRef::resource`].
    ///
    /// Carried through rather than re-derived from `plugin_id`, because
    /// `claim_displaced_plugins` accepts any unique, correctly-referencing id: it
    /// validates that the resource exists, that it is a framework plugin of this
    /// framework, and that its `plugin_id` matches — but not that its *name* is the
    /// canonical one. Re-deriving it downstream made the two helpers that mutate
    /// the receipt look up an id the receipt may not contain, and they fail after
    /// `apply_enable` has already persisted the new receipt, installed this
    /// adapter's own plugin and enabled it. Reading the id the receipt actually
    /// uses cannot diverge from it.
    resource: String,
    /// Framework-native plugin id the adapter disabled.
    plugin_id: String,
    /// Exclusive slot the plugin re-takes when restored, when declared.
    slot: Option<String>,
    /// Whether the framework command that performed the hand-off was issued —
    /// [`DisplacedPluginRef::applied`]. An unapplied entry is a recorded
    /// intention, not ownership, and nothing may act on it as though it were.
    applied: bool,
}

/// Resource id of a displaced framework plugin.
fn displaced_resource_id(plugin_id: &str) -> String {
    format!("{RES_DISPLACED_PREFIX}{plugin_id}")
}

/// `openclaw config get <key>` — a read-only probe of persisted framework
/// config. Never mutates, so it is safe in `prepare_enable` and `--dry-run`.
fn build_config_get_cmd(key: &str, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec!["config".to_string(), "get".to_string(), key.to_string()],
        home,
        user_home,
    )
}

/// `openclaw plugins disable <id>`.
fn build_disable_cmd(plugin_id: &str, home: &Path, user_home: Option<&Path>) -> FrameworkCommand {
    base_cmd(
        vec![
            "plugins".to_string(),
            "disable".to_string(),
            plugin_id.to_string(),
        ],
        home,
        user_home,
    )
}

/// Reduce a `config get` answer to its bare value token.
///
/// Hosts render the answer differently — a bare `false`, a JSON `"false"`, a
/// `key=value` line, or a preamble with the value on the last line — so take
/// the last non-empty line of stdout, keep only what follows the final `=` or
/// `:`, strip quoting and punctuation, and lower-case the result. This is the
/// same reduction the bundle's `install.sh` / `uninstall.sh` apply, so one
/// host's rendering classifies identically on both entry points.
fn config_answer_token(output: &CliOutput) -> String {
    let stdout = strip_ansi(&output.stdout);
    let Some(last) = stdout.lines().map(str::trim).rfind(|line| !line.is_empty()) else {
        return String::new();
    };
    let tail = match last.rsplit_once(['=', ':']) {
        Some((_, value)) => value,
        None => last,
    };
    tail.chars()
        .filter(|c| !matches!(c, '[' | ']' | '"' | '\'' | '`' | ',' | ';'))
        .collect::<String>()
        .trim()
        .to_lowercase()
}

/// Whether a reduced `config get` answer is a positive "off". Anything else —
/// including an empty answer — is *not* evidence of an operator's choice.
fn config_answer_is_false(token: &str) -> bool {
    matches!(token, "false" | "0" | "no" | "off" | "disabled")
}

/// Whether a reduced `config get` answer is a host's rendering of "nothing
/// here" rather than a value.
///
/// Hosts spell an unset key every way their serializer can — a bare word, a
/// JSON null, a Python repr, a placeholder dash — and none of those is evidence
/// of a choice. Compared case-insensitively: the token reaching this function
/// from [`config_answer_token`] is already lower-cased, but one read straight
/// out of a policy answer is not, and `NULL` means exactly what `null` does.
fn config_answer_is_vacant(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "" | "null"
            | "(null)"
            | "none"
            | "(none)"
            | "nil"
            | "undefined"
            | "nan"
            | "(empty)"
            | "unset"
            | "<unset>"
            | "n/a"
            | "-"
    )
}

/// Whether a reduced `plugins.slots.<slot>` answer is the operator's explicit
/// "this slot is off", as opposed to a key nobody ever set.
///
/// OpenClaw spells a deliberately closed slot as `plugins.slots.memory =
/// "none"`, and a host that answers with a boolean-ish off word means the same
/// thing. Both are a *choice*, and `plugins enable` re-runs the framework's
/// exclusive slot selection, so acting on either would silently pick a memory
/// backend the operator just declined. The off-words are shared with
/// [`config_answer_is_false`] on purpose: "off" must classify identically
/// whichever key it was read from, or one host's rendering would be honored on
/// the enablement probe and overridden on the slot probe.
fn slot_answer_is_explicit_off(token: &str) -> bool {
    token == "none" || config_answer_is_false(token)
}

/// Whether the host rules a displaced plugin out, and on whose authority.
/// See [`OpenClawDriver::displacement_block`].
///
/// The two policy variants are split because they are not the same claim, and
/// only one of them is about loading at all: [`Self::Denied`] is a plugin the
/// host refuses to load on every path it has *and* refuses to re-enable, while
/// [`Self::AllowlistExcluded`] is a plugin the host gates `plugins enable` on
/// without that settling whether it is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DisplacementBlock {
    /// Nothing rules it out, so the per-plugin enablement flag decides.
    None,
    /// A readable inventory no longer lists it.
    Absent,
    /// `plugins.deny` names it.
    Denied,
    /// A `plugins.allow` that names something omits it.
    AllowlistExcluded,
    /// The host's global plugin switch is off, so nothing loads.
    PluginsGloballyDisabled,
}

/// What [`OpenClawDriver::restore_decision`] concluded about handing one
/// displaced plugin back.
///
/// Every `Skip*` is a step-aside, not a failure: the real restore leaves
/// `cleanup_complete` alone for all of them, because retrying can never succeed
/// and reporting otherwise would strand the receipt forever with this adapter's
/// own plugin already uninstalled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RestoreDecision {
    /// Issue `plugins enable`.
    Restore,
    /// The receipt records the displacement but the hand-off never ran, so this
    /// adapter never disabled the plugin and has nothing to hand back.
    SkipNotApplied,
    /// The host's inventory no longer has it, so there is nothing to hand back.
    SkipAbsent,
    /// `plugins.deny` names it: the host neither loads it nor accepts the
    /// `plugins enable` a restore would issue.
    SkipDenied,
    /// A restrictive `plugins.allow` excludes it, so the host gates the restore on
    /// a key only the operator should edit.
    ///
    /// Not the same claim as [`Self::SkipDenied`], and the difference is the whole
    /// reason the two are variants rather than one key string: an allowlist says
    /// nothing about whether the plugin is *loaded* — OpenClaw has two load paths
    /// that never consult it — so this is not "policy keeps it off" but "the
    /// restore is gated on operator policy". Whether the gate refuses outright
    /// (v2026.9.2 `src/plugins/enable.ts:38-40`, exiting non-zero through
    /// `src/cli/plugins-cli.runtime.ts:218-223`) or opens by appending the id to
    /// the allowlist (v2026.4.14 `src/plugins/enable.ts:12-24`) is a host-version
    /// question, and either way editing the operator's allowlist to force a
    /// restore is not this driver's call.
    SkipAllowlistExcluded,
    /// The host's global plugin switch is off, so it would refuse every restore.
    SkipPluginsGloballyDisabled,
    /// The operator closed its exclusive slot outright.
    SkipSlotClosed { slot: String, sentinel: String },
    /// The operator gave its exclusive slot to a third plugin.
    SkipSlotOwned { slot: String, owner: String },
}

/// One dry-run line predicting a restore, from the decision the real path will
/// make. `context` distinguishes the three previews that share this formatter:
/// a plain disable, a displacement the contract being enabled dropped, and a
/// restore in a prior state directory during migration.
fn restore_preview_line(
    decision: &RestoreDecision,
    plugin_id: &str,
    scope: &str,
    why: &str,
) -> String {
    match decision {
        RestoreDecision::Restore => {
            format!(
                "would re-enable openclaw plugin '{plugin_id}'{scope}, {why}, as the host reads now"
            )
        }
        RestoreDecision::SkipNotApplied => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} alone: the enable that recorded \
             this displacement failed before the hand-off ran, so this adapter never disabled it \
             and there is nothing to hand back"
        ),
        RestoreDecision::SkipAbsent => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} alone: it is no longer in this \
             host's `plugins list` inventory, so the displacement is already released"
        ),
        RestoreDecision::SkipDenied => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} disabled: `plugins.deny` names it, \
             so OpenClaw neither loads it nor accepts the restore and the displacement counts \
             as released"
        ),
        RestoreDecision::SkipAllowlistExcluded => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} disabled: a restrictive \
             `plugins.allow` excludes it, so the restore is gated on operator policy this driver \
             will not edit, and the displacement counts as released"
        ),
        RestoreDecision::SkipPluginsGloballyDisabled => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} disabled: `plugins.enabled` is \
             false, so OpenClaw would refuse every restore and the displacement counts as \
             released"
        ),
        RestoreDecision::SkipSlotClosed { slot, sentinel } => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} disabled: plugins.slots.{slot} is \
             explicitly '{sentinel}', which closes the slot"
        ),
        RestoreDecision::SkipSlotOwned { slot, owner } => format!(
            "would leave openclaw plugin '{plugin_id}'{scope} disabled: plugins.slots.{slot} now \
             belongs to '{owner}'"
        ),
    }
}

/// The conditions under which a *later* `disable` will hand a displaced plugin
/// back, described from the declaration alone.
///
/// `plan_enable` cannot run [`OpenClawDriver::restore_decision`]: that reads the
/// host as it is now, while this note describes a disable that has not happened
/// yet, and rendering a present-tense prediction as a future promise would be a
/// fresh way to be wrong. So it enumerates the branch set instead — every
/// `RestoreDecision::Skip*` that describes the *host*, with the slot vetoes only
/// when a slot is declared — and it is one function precisely so the enumeration
/// cannot drift from the decision it describes. A hand-written note here once
/// covered only the slot vetoes and called a slotless restore "unconditional",
/// which the inventory and policy vetoes are not: neither looks at the slot.
///
/// [`RestoreDecision::SkipNotApplied`] is deliberately absent, and it is the only
/// variant that is. It is not a condition the host can reach between enable and
/// disable; it records that *this* enable failed before its hand-off ran. An
/// enable that failed has no successful hand-off for a later disable to undo, and
/// the preview that does have to describe that state — the disable-side one —
/// renders it from `restore_decision` directly. Naming it here would tell an
/// operator about a branch the operation they are previewing cannot take.
///
/// Adding any other `RestoreDecision::Skip*` variant means adding it here too;
/// `restore_conditions_note_covers_every_veto` fails otherwise.
fn restore_conditions_note(slot: Option<&str>) -> String {
    let always = "unless by then it has left this host's plugin inventory, `plugins.enabled` has \
                  been set to false, `plugins.deny` names it, or a restrictive `plugins.allow` \
                  excludes it";
    match slot {
        Some(slot) => format!(
            ", and hand it back on disable {always}, or unless plugins.slots.{slot} has been \
             given to another plugin or explicitly closed"
        ),
        None => format!(", and hand it back on disable {always}"),
    }
}

/// What the slot a displaced plugin names says about who turned it off.
///
/// Four states rather than a boolean, because the negative answers are not the same
/// claim and only some of them may drive the destructive direction — deleting a
/// claim, or disowning a hand-off:
///
/// - [`Self::Own`] is positive evidence for this driver.
/// - [`Self::Other`] and [`Self::NotDeclared`] are positive evidence *against* it,
///   from the host and from the contract respectively.
/// - [`Self::Unreadable`] is no evidence at all, and must never be spent as if it
///   were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotAttribution {
    /// The slot names this adapter's own plugin, which is the second half of the
    /// write OpenClaw's slot selection performs: this driver is a sufficient
    /// explanation for the plugin being off.
    Own,
    /// The slot is readable and names something else — another plugin, the displaced
    /// plugin itself, an explicit off sentinel, or no selection at all.
    Other,
    /// The receipt records no slot for this entry, so there is nothing to ask.
    ///
    /// Not the same as [`Self::Unreadable`], and the difference is the whole point:
    /// this is an answer, taken from the contract that produced the receipt.
    /// `DisplacedPluginSpec::slot` is documented as "declare it when the displaced
    /// plugin re-takes an exclusive slot, omit it when it does not", and OpenClaw's
    /// `applyExclusiveSlotSelection` only ever disables plugins of the *selected
    /// plugin's own kind* — the ones competing for that kind's slot. So a
    /// declaration naming no slot says this plugin does not compete for one, and
    /// this driver's own selection cannot be what turned it off.
    NotDeclared,
    /// The host could not answer the probe.
    Unreadable,
}

/// What [`OpenClawDriver::handoff_attribution`] concluded about one receipt entry —
/// three answers rather than a boolean, for the same reason [`SlotAttribution`] has
/// four: the negative and the unreadable are not the same claim, and only some
/// callers may act on the negative.
///
/// - [`Self::Performed`] is positive evidence for this driver — the slot names its
///   own plugin *and* the plugin's own flag reads off, which is a state OpenClaw's
///   slot selection produces by construction.
/// - [`Self::NotPerformed`] is positive evidence against it, from a readable slot
///   naming somebody else, a contract that declared no slot, or a flag reading on.
/// - [`Self::Unknown`] is no evidence at all. A caller that spends it as
///   `NotPerformed` turns one transient `config get` failure into an ownership
///   decision — see [`OpenClawDriver::recovered_prior_handoff`] for what that cost
///   a re-enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffAttribution {
    /// This adapter's own slot selection is a sufficient explanation for the
    /// plugin being off.
    Performed,
    /// The host answered, and the answer rules this adapter out.
    NotPerformed,
    /// The host could not answer one of the two halves.
    Unknown,
}

/// Whether a re-enable recovers an ownership the prior receipt never marked.
///
/// The verdict of [`OpenClawDriver::recovered_prior_handoff`], which both
/// `prepare_enable` and `plan_enable` ask so that the receipt and its own preview
/// cannot drift apart. There is no third variant for "could not tell": that answer
/// is an error, because both directions the caller could go from it are destructive
/// and the evidence to choose between them is exactly what the host withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PriorHandoffRecovery {
    /// Positive evidence that this adapter's own slot selection performed the
    /// hand-off, so the replacement receipt carries the entry as already applied
    /// and the preview says the ownership is carried over.
    Recovered,
    /// Nothing to recover: no prior receipt for this instance names the plugin as
    /// unapplied, or the host positively rules this adapter out. Either way this
    /// enable claims nothing and a later `disable` restores nothing.
    NothingToRecover,
}

/// What a fresh probe of one declared displacement concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DisplacementProbe {
    /// The plugin is there and positively *on*, so disabling it is a transition
    /// this enable makes and can attribute to itself. `apply_enable` re-confirms
    /// these before mutating.
    ClaimEnabled,
    /// The host could not say whether the plugin was on. Still claimed — see the
    /// asymmetry in [`OpenClawDriver::displacement_probe`] — but the probe alone
    /// cannot attribute it, so whether `apply_enable` may re-confirm it is settled
    /// by [`inherited_displacement_ids`]: a prior receipt for this same state
    /// directory that already owns the id makes the claim inherited and
    /// off-limits, and with nothing to inherit the claim is this enable's own and
    /// is re-confirmed like any other.
    ///
    /// Both directions lose something real, which is why the split runs along
    /// ownership rather than along the probe result:
    ///
    /// - Re-confirming an **inherited** claim loses ownership. A re-enable whose
    ///   prepare-time probe happens to fail once finds the plugin already off, and
    ///   the only honest explanation is that a *previous* enable of this same
    ///   adapter turned it off; the re-confirm reads that `false` as "somebody else
    ///   just closed it" and deletes the claim from the replacement receipt.
    ///   `preserve_reenable_facts` already declined to re-add the fact because the
    ///   fresh claim occupied the same resource, and once the re-enable succeeds
    ///   the prior receipt is gone — so the ownership is lost permanently and a
    ///   later `adapter disable` never hands the plugin back.
    /// - *Not* re-confirming a **first** enable's claim re-opens the takeover
    ///   window the re-confirm exists to close. There is no older ownership to
    ///   protect, so a `false` at apply time can only mean the plugin went off for
    ///   a reason this enable never observed; keeping the claim records a
    ///   transition this adapter did not make, the `plugins disable` it then runs
    ///   changes nothing, and a later `adapter disable` re-enables a plugin the
    ///   operator had closed themselves — with no trace left of why.
    ClaimUnverified,
    /// Somebody or something else already keeps it off, and the reason — so the
    /// plan and the receipt can say which, rather than a bare "not claimed".
    ///
    /// Restricted to an explanation no command this driver runs can produce: a
    /// policy key an operator or the framework's own config set. The plugin's own
    /// enablement flag is [`Self::FlagDisabled`], not this, precisely because that
    /// one *is* a state ANOLISA's own commands write.
    AlreadyOff(String),
    /// The plugin's own persisted `plugins.entries.<id>.enabled` reads `false`, and
    /// no policy explains it.
    ///
    /// Kept apart from [`Self::AlreadyOff`] because this is the one answer that says
    /// nothing about *who* turned the plugin off once this enable has run anything:
    /// OpenClaw's `plugins install` and `plugins enable` both apply exclusive slot
    /// selection (`applySlotSelectionForPlugin` → `applyExclusiveSlotSelection` in
    /// v2026.4.14), which takes the slot for the plugin being enabled and writes
    /// `enabled = false` for every other plugin of the same kind. Attributing it
    /// needs the corroborating half of that write — see
    /// [`OpenClawDriver::slot_selection_explains_off`].
    FlagDisabled,
    /// The host's inventory does not have it at all.
    NotOnHost,
}

/// Which explicit policy, if any, the driver could positively read for one
/// framework plugin.
///
/// Split from a bare `Option<String>` because the global switch and a per-plugin
/// list need different sentences: "remove it from `plugins.deny`" is advice, and
/// "remove it from `plugins.enabled`" is nonsense.
///
/// The allowlist is a variant of its own rather than a second [`Self::Key`],
/// because it is the one reading whose *meaning* depends on the question being
/// asked — see [`AllowlistMeaning`] and
/// [`OpenClawDriver::displacement_probe`]. `plugins.deny` and `plugins.enabled`
/// mean the same thing everywhere: OpenClaw's own activation decision honors both
/// on every load path it has, and `enablePluginInConfig` refuses a
/// `plugins enable` against both.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PolicyBlock {
    /// No policy the driver could positively read keeps it off.
    None,
    /// Named by `plugins.deny`.
    ///
    /// Carries no key string, because after the allowlist became its own reading
    /// there is exactly one key that can produce it.
    Denied,
    /// Omitted from a `plugins.allow` that names something.
    Allowlist,
    /// The host's global plugin switch is off, so *every* `plugins enable` is
    /// refused.
    PluginsDisabled,
}

/// Whether a restrictive `plugins.allow` that omits the plugin settles the
/// question an [`OpenClawDriver::displacement_block`] caller is asking.
///
/// The two callers ask different questions, and the allowlist answers only one of
/// them. [`OpenClawDriver::policy_blocking_plugin`] reports the reading; this says
/// what weight the caller puts on it, and [`OpenClawDriver::displacement_probe`]
/// documents the upstream evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowlistMeaning {
    /// Asking whether the host would refuse the `plugins enable` a restore issues.
    /// A restrictive allowlist is exactly that refusal on hosts that gate
    /// enablement on it, so it vetoes and the displacement counts as released.
    RefusesRestore,
    /// Asking whether the plugin can hold this adapter's tool names right now. A
    /// restrictive allowlist does not settle that — two of the host's own load
    /// paths ignore it — so it is not a block and the caller falls through to the
    /// evidence that does decide.
    DoesNotKeepItOff,
}

/// *When* a decision reads `plugins.allow`: as the host holds it now, or as it
/// will hold it once the operation's own `plugins uninstall` has run.
///
/// Orthogonal to [`AllowlistMeaning`], which asks what weight the reading
/// carries. This one asks which reading is the right one to weigh, and it exists
/// because `plugins.allow` is the single key this driver's own cleanup rewrites:
/// `removePluginFromConfig` drops the uninstalled plugin's id from it and removes
/// the key entirely once nothing is left in it (v2026.4.14
/// `src/plugins/uninstall.ts:126-133`). Nothing else the restore reads behaves
/// that way — `plugins.deny` and `plugins.enabled` are operator keys this driver
/// never writes, and the inventory and slot readings are *meant* to be taken
/// after the uninstall.
///
/// A decision made after the uninstall therefore reads the key live and gets the
/// post-uninstall answer for free, while a preview of that same decision runs
/// before it and reads a key that still names this adapter's own plugin. Feeding
/// the preview's reading to the preview's verdict is what made the two contradict
/// each other: for an allowlist naming only this adapter's plugin — the shape a
/// host is left in once it has admitted this adapter and nothing else — the plan
/// reported a restriction the operation was about to delete, promised to leave the
/// bundled plugin disabled, and then re-enabled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowlistReading<'a> {
    /// Read the key as the host holds it.
    ///
    /// Correct for every decision taken after the uninstall has already run — the
    /// real restore in `disable`, and the cross-home `cleanup_replaced_claim`
    /// branch, which delegates to one — and for every decision no uninstall
    /// precedes at all: the enable-side probe, `status`, and the same-home
    /// dropped-displacement cleanup, which restores from the prior receipt while
    /// this adapter's own plugin stays installed.
    Live,
    /// Project the key past the removal of this adapter's own plugin, which the
    /// operation being described uninstalls before the restore it predicts.
    ///
    /// Carries that plugin's id because only its own entry leaves the key;
    /// `None` is a receipt recording no plugin of its own, which uninstalls
    /// nothing and so projects to the live reading.
    AfterOwnUninstall(Option<&'a str>),
}

impl AllowlistReading<'_> {
    /// The ids `plugins.allow` still names when this decision is acted on.
    ///
    /// An empty result is the projected form of a vacant key, not of a
    /// restrictive one: upstream drops the key when the filter empties the array,
    /// so a restriction whose only entry was this adapter's own plugin is gone by
    /// the time the restore asks.
    fn surviving_allow_ids(&self, ids: Vec<String>) -> Vec<String> {
        let AllowlistReading::AfterOwnUninstall(Some(own)) = self else {
            return ids;
        };
        ids.into_iter().filter(|id| id != own).collect()
    }
}

/// What a `config get` answer for a plugin-id *list* key names.
#[derive(Debug, PartialEq, Eq)]
enum PolicyIdList {
    /// The host named these plugin ids.
    Named(Vec<String>),
    /// The host answered and named nothing — unset, null, or an empty list.
    Vacant,
}

/// Read a `config get` answer whose value is a list of plugin ids.
///
/// OpenClaw renders an array as **pretty JSON**: one quoted element per line
/// inside `[` … `]`. Neither reading this file already had can see that shape,
/// and both fail in the direction that matters:
///
/// - [`list_contains_plugin`] matches whole whitespace tokens, so an element
///   line `"memory-core",` — quotes and trailing comma included — is not a match,
///   and a real denylist would be read as naming nobody;
/// - [`config_answer_token`] reduces to the *last* non-empty line, which for a
///   pretty array is `]`; stripped of punctuation that is empty, so a real
///   allowlist would be read as vacant and therefore as no restriction at all.
///
/// So the JSON is parsed first, and the line reading is only the fallback for a
/// host that answers with something else. Both readings keep the same rule: only
/// a list that names something is evidence, because guessing "restrictive" from a
/// vacant answer would switch the whole hand-off off wherever the key was merely
/// never set.
///
/// The fallback needs `key` because a `config get` answer mixes the value with
/// whatever the host felt like printing first, and only the key says which is
/// which. Reading *every* line as a value list is what turned
///
/// ```text
/// reading policy...
/// plugins.allow = null
/// ```
///
/// into an allowlist naming `reading` and `policy...`: no `[` or `{` for the JSON
/// reader to anchor on, the first line has no separator so its whole text looked
/// like a bare value, and the second line's `null` was correctly vacant — leaving
/// the preamble as the answer. `policy_blocking_plugin` then read that as a
/// restrictive allowlist omitting the plugin — which at the time made enable skip
/// the displacement entirely, so both plugins kept fighting over the tool names
/// with no ownership recorded for `status` to check, and disable printed
/// instructions to edit a key whose value is `null`. Enable no longer treats an
/// allowlist as a veto, but the misreading is not thereby harmless: the same answer
/// at *restore* time gates a `plugins enable` the host would have accepted, leaves
/// the bundled plugin disabled for good, and sends the operator to edit a key that
/// was never set.
///
/// Residual, stated plainly: a host that prints only prose, no value line, and
/// still exits 0 would have its prose read as ids. Every documented rendering —
/// bare value, JSON, `key=value`, preamble with the value last — is handled; that
/// one is not, and telling it apart from a genuine bare value list is not
/// possible by shape alone.
fn policy_id_list(output: &CliOutput, key: &str) -> PolicyIdList {
    if let Some(items) = json_id_array(&output.stdout) {
        return if items.is_empty() {
            PolicyIdList::Vacant
        } else {
            PolicyIdList::Named(items)
        };
    }

    let stripped = strip_ansi(&output.stdout);
    let mut echoed_values: Vec<&str> = Vec::new();
    let mut bare_values: Vec<&str> = Vec::new();
    for line in stripped.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line.rsplit_once(['=', ':']) {
            // This key's own echo: the value is whatever follows.
            Some((head, tail)) if head.trim() == key => echoed_values.push(tail),
            // Some other left-hand side — `warning: config not found`, a
            // sentence with a colon in it. Prose, not a value; drop the line
            // rather than guessing which words were meant.
            Some(_) => {}
            // No separator at all: a bare value line.
            None => bare_values.push(line),
        }
    }
    // Believe the echo when the host gave one, and read nothing else. Mixing the
    // two is exactly how the preamble above became the answer.
    let source = if echoed_values.is_empty() {
        &bare_values
    } else {
        &echoed_values
    };

    let mut names: Vec<String> = Vec::new();
    for text in source {
        for raw in text.split(|c: char| c.is_whitespace() || c == ',') {
            let token = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | '[' | ']' | ';'));
            if token.is_empty() || config_answer_is_vacant(token) {
                continue;
            }
            // A value is a list of plugin ids, so a token that cannot be one is
            // junk from the host's rendering and is dropped. A shape check, not a
            // prose filter: the key-echo rule above is what keeps diagnostics out.
            if validate_plugin_id(token).is_err() {
                continue;
            }
            names.push(token.to_string());
        }
    }
    if names.is_empty() {
        PolicyIdList::Vacant
    } else {
        PolicyIdList::Named(names)
    }
}

/// The string elements of a JSON array in `stdout`, when it holds one.
///
/// Accepts a bare array, an array under a `value` envelope, and a JSON `null`
/// (a host's rendering of "unset", which is an empty list rather than an
/// unparseable answer). Diagnostics printed before the JSON are tolerated by
/// scanning for the opening bracket, the same way [`extract_trailing_json`]
/// scans for an opening brace — that helper cannot be reused here because it
/// only looks for objects.
fn json_id_array(stdout: &str) -> Option<Vec<String>> {
    fn items_of(value: &serde_json::Value) -> Option<Vec<String>> {
        let array = value
            .as_array()
            .or_else(|| value.get("value").and_then(serde_json::Value::as_array))?;
        Some(
            array
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        )
    }

    let trimmed = stdout.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(items) = items_of(&value) {
            return Some(items);
        }
        if value.is_null() {
            return Some(Vec::new());
        }
    }
    for (idx, _) in stdout.char_indices().filter(|&(_, c)| c == '[' || c == '{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout[idx..].trim())
            && let Some(items) = items_of(&value)
        {
            return Some(items);
        }
    }
    None
}

/// `plugins.slots.<slot>` readings keyed by slot name, taken before a mutating
/// step of this driver's own could rewrite them.
///
/// `None` is a reading the host could not answer, which is *not* the same as a
/// slot this map never covered — see [`SlotReadings`].
type CapturedSlots = BTreeMap<String, Option<String>>;

/// Where the `plugins.slots.<slot>` reading a decision reasons from comes from.
///
/// Almost every caller wants [`Self::Live`]: a preview, `status`, the write-ahead
/// mark and the apply-time re-confirm all read a host nothing has mutated yet.
/// A real `disable` is the one exception, and it is not a small one. Its first
/// step is `plugins uninstall` of this adapter's own plugin, and OpenClaw's
/// `removePluginFromConfig` rewrites config as it goes: a memory slot the
/// uninstalled plugin owned is reset to that slot's default — `memory-core`, from
/// `DEFAULT_SLOT_BY_KEY` in `src/plugins/slots.ts:17-20` — in v2026.4.14
/// `src/plugins/uninstall.ts:147-155`. Since this adapter's own plugin is what
/// owns `plugins.slots.memory` for the whole life of the receipt, the slot the
/// restore reasons about three steps later is one this disable just wrote.
///
/// One decision reads the slot as evidence about the *past*, and it comes out
/// wrong on the rewritten value: the hand-off attribution behind
/// [`RestoreDecision::SkipNotApplied`] needs the slot as it was when the hand-off
/// was formed — naming this adapter's own plugin — and instead finds
/// `memory-core`, reads that as somebody else's selection, and drops the only
/// receipt for a plugin this adapter really turned off, reporting a complete
/// cleanup over a backend it just left switched off.
///
/// So `disable` captures the readings of its *unapplied* entries before it mutates,
/// hands them to the attribution, and writes the ownership they establish back into
/// the receipt — a retry has no capture that can see it, so the record is the only
/// place it can survive. Everything else keeps its live read, and the split is the
/// question each is asking rather than an oversight: the slot veto wants the owner
/// as it stands when the `plugins enable` would run, and the policy
/// keys want the allowlist as the uninstall left it — `removePluginFromConfig` also
/// drops the uninstalled plugin from `plugins.allow` (v2026.4.14
/// `src/plugins/uninstall.ts:126-133`), and whether a restriction survives that is
/// exactly what decides if the host would refuse the restore.
#[derive(Debug, Clone, Copy)]
enum SlotReadings<'a> {
    /// Ask the host now.
    Live,
    /// Reason from readings taken before this operation's own mutation.
    Captured(&'a CapturedSlots),
}

/// The host readings one [`OpenClawDriver::restore_decision`] is made against.
///
/// Bundled because all four are resolved once per operation and then asked about
/// once per displaced plugin, and because two of them say *when* a reading was
/// taken rather than what it says: `plugins uninstall` rewrites both
/// `plugins.slots.<slot>` and `plugins.allow`, so the two sides of that uninstall
/// do not read the same host. Keeping the two corrections in one type is what stops
/// a caller from applying one and forgetting the other — which is how a preview
/// came to promise the opposite of the operation it described.
#[derive(Debug, Clone, Copy)]
struct RestoreHost<'a> {
    /// This adapter's own plugin id, from the receipt being restored. `None` when
    /// the receipt records no plugin of its own.
    own_plugin_id: Option<&'a str>,
    /// `plugins list` stdout, or `None` when the host could not answer.
    inventory: Option<&'a str>,
    /// Where the two slot readings come from — see [`SlotReadings`].
    slots: SlotReadings<'a>,
    /// Which `plugins.allow` to weigh — see [`AllowlistReading`].
    allowlist: AllowlistReading<'a>,
}

/// What the persisted `plugins.entries.<id>.enabled` flag says about a
/// framework plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginEnablement {
    /// Positively enabled, or absent — the bundled default.
    Enabled,
    /// Positively disabled.
    Disabled,
    /// The host could not answer.
    Unknown,
}

/// What a reduced `plugins.slots.<slot>` answer means for handing a displaced
/// plugin back.
#[derive(Debug, PartialEq, Eq)]
enum SlotRestore {
    /// Nothing blocks the restore.
    Proceed,
    /// The operator explicitly closed the slot after this adapter displaced the
    /// plugin; restoring it would re-open the slot behind their back.
    ExplicitlyOff(String),
    /// A third plugin owns the slot now.
    OwnedByThird(String),
}

/// Classify a reduced `plugins.slots.<slot>` answer.
///
/// `answer` is `None` when the host could not answer at all, which is *not* the
/// same as an answer that reads empty: an unanswerable probe is no evidence
/// either way, while a readable one can be an explicit off. Everything short of
/// a positively identified third owner or an explicit off allows the restore:
/// no owner at all, a host that renders "unset" as a bare word, this adapter's
/// own plugin, or the displaced plugin itself.
fn slot_restore_decision(
    answer: Option<&str>,
    own_plugin_id: Option<&str>,
    displaced_id: &str,
) -> SlotRestore {
    let Some(token) = answer else {
        return SlotRestore::Proceed;
    };
    // A plugin this receipt already recognizes is an owner, never a sentinel —
    // a bundled plugin really named `none` still holds its own slot.
    if token == displaced_id || Some(token) == own_plugin_id {
        return SlotRestore::Proceed;
    }
    if slot_answer_is_explicit_off(token) {
        return SlotRestore::ExplicitlyOff(token.to_string());
    }
    if config_answer_is_vacant(token) {
        SlotRestore::Proceed
    } else {
        SlotRestore::OwnedByThird(token.to_string())
    }
}

/// Resolve a receipt's displaced-plugin references to plugin ids and slots.
///
/// A dangling or mistyped reference is a corrupted receipt and fails closed:
/// restoring nothing would silently leave a bundled plugin disabled, and
/// restoring the wrong one would take a slot from its owner.
///
/// The slot constraints the Manager applies to a *contract* are re-applied to the
/// *receipt* here, because the receipt is what disable actually consumes and it
/// can be edited without the contract ever being re-read. Two entries sharing one
/// exclusive slot are not merely redundant: disable restores the first, the guard
/// then reads that plugin as a **third** owner for the second and steps aside, and
/// the receipt is still removed as a completed cleanup — leaving a plugin this
/// adapter disabled with nothing recording why. An empty slot is rejected for the
/// same reason the contract rejects it: `plugins.slots.` is not a key, and an
/// entry that names one silently degrades to an unguarded restore.
fn claim_displaced_plugins(claim: &AdapterClaim) -> Result<Vec<DisplacedPlugin>, AdapterError> {
    let DriverPayload::OpenClaw(payload) = &claim.driver_payload else {
        return Ok(Vec::new());
    };
    let own_plugin_id = claim_own_plugin(claim)?;
    let mut seen_resources: HashSet<&str> = HashSet::new();
    let mut seen_plugin_ids: HashSet<&str> = HashSet::new();
    // Slot -> the plugin that already claimed it, so a collision can name both.
    let mut slots_taken: BTreeMap<&str, &str> = BTreeMap::new();
    let mut resolved = Vec::with_capacity(payload.displaced_plugins.len());
    for entry in &payload.displaced_plugins {
        if !seen_resources.insert(entry.resource.as_str()) {
            return Err(invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin reference '{}' appears more than once",
                    entry.resource
                ),
            ));
        }
        let resource = claim.resource(&entry.resource).ok_or_else(|| {
            invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin reference '{}' has no resource",
                    entry.resource
                ),
            )
        })?;
        // Purpose and framework both matter, not just the resource shape. A
        // reference to this adapter's *own* plugin resource — or to another
        // framework's plugin — is still a `FrameworkPlugin`, and honoring it
        // would drive `openclaw plugins enable` against a plugin this receipt
        // never displaced.
        if resource.purpose != PURPOSE_DISPLACED_PLUGIN {
            return Err(invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin reference '{}' has purpose '{}', not \
                     '{PURPOSE_DISPLACED_PLUGIN}'",
                    entry.resource, resource.purpose
                ),
            ));
        }
        let ClaimResourceKind::FrameworkPlugin {
            framework,
            plugin_id,
        } = &resource.kind
        else {
            return Err(invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin reference '{}' is not a framework plugin resource",
                    entry.resource
                ),
            ));
        };
        if framework != &claim.framework {
            return Err(invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin reference '{}' belongs to framework '{framework}', not '{}'",
                    entry.resource, claim.framework
                ),
            ));
        }
        validate_plugin_id(plugin_id).map_err(|err| {
            invalid_displaced_claim(claim, &format!("displaced plugin id: {err}"))
        })?;
        if Some(plugin_id.as_str()) == own_plugin_id.as_deref() {
            return Err(invalid_displaced_claim(
                claim,
                &format!(
                    "displaced plugin '{plugin_id}' is this adapter's own plugin; restoring it \
                     would fight the adapter it belongs to"
                ),
            ));
        }
        if !seen_plugin_ids.insert(plugin_id.as_str()) {
            return Err(invalid_displaced_claim(
                claim,
                &format!("displaced plugin '{plugin_id}' is claimed more than once"),
            ));
        }
        if let Some(slot) = entry.slot.as_deref() {
            if slot.is_empty() {
                return Err(invalid_displaced_claim(
                    claim,
                    &format!(
                        "displaced plugin '{plugin_id}' names an empty slot; `plugins.slots.` is \
                         not a key, and the entry would restore it with no guard at all"
                    ),
                ));
            }
            validate_config_key(&format!("plugins.slots.{slot}")).map_err(|err| {
                invalid_displaced_claim(
                    claim,
                    &format!("displaced plugin '{plugin_id}' slot: {err}"),
                )
            })?;
            if let Some(other) = slots_taken.get(slot) {
                return Err(invalid_displaced_claim(
                    claim,
                    &format!(
                        "displaced plugins '{other}' and '{plugin_id}' both claim the exclusive \
                         slot '{slot}'; restoring the first would make it a third owner for the \
                         second, which disable would then leave behind"
                    ),
                ));
            }
            slots_taken.insert(slot, plugin_id.as_str());
        }
        resolved.push(DisplacedPlugin {
            resource: entry.resource.clone(),
            plugin_id: plugin_id.clone(),
            slot: entry.slot.clone(),
            applied: entry.applied,
        });
    }
    Ok(resolved)
}

/// Just the plugin ids of [`claim_displaced_plugins`], for the enable path
/// where the slot is irrelevant.
fn claim_displaced_plugin_ids(claim: &AdapterClaim) -> Result<Vec<String>, AdapterError> {
    Ok(claim_displaced_plugins(claim)?
        .into_iter()
        .map(|entry| entry.plugin_id)
        .collect())
}

/// The ids of a receipt's displacements whose hand-off **actually ran**.
///
/// Ownership is the applied subset, and every set that decides what a *later*
/// operation may rely on has to be built from it. An entry recorded by an enable
/// that failed before `plugins disable` is a declaration of intent, not a
/// transition this adapter performed — which is why
/// [`RestoreDecision::SkipNotApplied`] and the `status` probe both refuse to act
/// on one.
///
/// Inheriting an unapplied id is not a harmless duplicate of that refusal, because
/// the inheritance is what suppresses the re-confirmation: the id arrives in the
/// replacement receipt as an entry no probe of this host produced, so it is not in
/// `freshly_claimed`, so `apply_displacements` skips it, marks it applied, and runs
/// a `plugins disable` that exits 0 having changed nothing. From then on the
/// receipt claims a transition nobody made, and `adapter disable` re-enables a
/// plugin the operator closed themselves between the two enables.
///
/// [`claim_displaced_plugin_ids`] stays unfiltered on purpose:
/// `dropped_displaced_plugins` uses it to ask which resources the replacement
/// receipt *holds*, which is a question about the receipt's shape and not about
/// whether the hand-off ran.
fn claim_applied_displacement_ids(claim: &AdapterClaim) -> Result<Vec<String>, AdapterError> {
    Ok(claim_displaced_plugins(claim)?
        .into_iter()
        .filter(|entry| entry.applied)
        .map(|entry| entry.plugin_id)
        .collect())
}

/// The displaced plugin ids a validated prior receipt already owns **in the
/// OpenClaw instance this operation resolves to**.
///
/// This is the ownership an unverified claim may inherit. A host that cannot
/// answer whether a plugin is on leaves the claim resting on the asymmetry in
/// [`OpenClawDriver::displacement_probe`] alone, so who the later `false` belongs
/// to has to come from somewhere else — and the only honest source is a receipt
/// that already says this adapter turned that plugin off.
///
/// It goes through the same gate as [`preserve_openclaw_displaced_facts`], because
/// it answers the same question: ownership does not travel across OpenClaw state
/// directories. Inheriting from a receipt written against another instance would
/// suppress the re-confirmation that instance's own operator changes deserve, and
/// the failure is the one that gate exists to prevent — a later disable re-enables
/// a plugin the operator had closed.
///
/// Empty for a first enable, and that is the point: with no prior ownership to
/// protect, an unverified claim is this enable's own, and a `false` read at apply
/// time can only mean the plugin went off for a reason this enable never observed.
/// Empty too for a prior whose entries never reached their hand-off, for the same
/// reason — an enable that failed before `plugins disable` ran established no
/// ownership for this one to inherit.
///
/// # Errors
///
/// Propagates a receipt-consistency error from the prior's own displaced-plugin
/// references, or an unresolvable state directory.
fn inherited_displacement_ids(
    prior: Option<&AdapterClaim>,
    ctx: &DriverCtx,
) -> Result<HashSet<String>, AdapterError> {
    let Some(prior) = prior_for_this_instance(prior, ctx)? else {
        return Ok(HashSet::new());
    };
    // Applied entries only: see [`claim_applied_displacement_ids`]. An unapplied
    // one is not ownership to protect, and treating it as such is what exempts it
    // from the re-confirmation that would otherwise notice the operator's own
    // disable. What an unapplied entry *can* still be is a hand-off the receipt
    // failed to mark, which is a different question with a different answer — see
    // [`prior_unrecorded_displacements`].
    Ok(claim_applied_displacement_ids(prior)?.into_iter().collect())
}

/// The prior receipt when it describes the OpenClaw instance this operation
/// resolves to, and `None` when there is nothing to inherit from.
///
/// One gate rather than a copy per caller, because every ownership question on the
/// enable side rests on the same fact: ownership does not travel across OpenClaw
/// state directories. [`preserve_openclaw_displaced_facts`] states why at length
/// and applies it to the facts it merges; this applies it to the two questions
/// asked *before* the replacement receipt exists — which claims an unverified probe
/// may treat as already owned, and which unrecorded hand-offs may be recovered.
///
/// # Errors
///
/// Propagates an unresolvable state directory, from the prior receipt or from this
/// operation's own context.
fn prior_for_this_instance<'a>(
    prior: Option<&'a AdapterClaim>,
    ctx: &DriverCtx,
) -> Result<Option<&'a AdapterClaim>, AdapterError> {
    let Some(prior) = prior else {
        return Ok(None);
    };
    // A receipt written by another framework owns no OpenClaw plugin.
    // `claim_state_dir` would reject it; answering "no prior" is the same verdict
    // without failing an enable over a receipt this driver has no business reading.
    if !matches!(prior.driver_payload, DriverPayload::OpenClaw(_)) {
        return Ok(None);
    }
    if claim_state_dir(prior)? != require_home(ctx)? {
        return Ok(None);
    }
    Ok(Some(prior))
}

/// The displaced plugins a prior receipt for **this same OpenClaw instance**
/// records without having marked the hand-off, keyed by plugin id.
///
/// The complement of [`inherited_displacement_ids`], and the reason it exists is
/// that an unapplied entry is not automatically an unperformed one. Writing the
/// mark needs a readable `plugins.slots.<slot>`, so a probe the host cannot answer —
/// or a process killed before the save that follows it — leaves a receipt reading
/// `applied = false` over a plugin this adapter really did turn off. Three places
/// already recover it from positive host evidence: the enable-side mark and the
/// restore branch that refuses to strand the plugin, both through
/// [`OpenClawDriver::handoff_performed_by_slot_selection`], and the one a `disable`
/// writes from its pre-uninstall capture, which asks the tri-state instead because
/// what follows it destroys the evidence — see
/// [`OpenClawDriver::recovered_disable_attribution`]. This is the set a *fourth*
/// place has to ask about, because a re-enable is the one path where the receipt the
/// evidence would be recovered into is itself about to be replaced: `prepare_enable`
/// asks,
/// and it is the caller that has to, being the only scope holding both the prior
/// receipt and this round's probe — the same reason [`inherited_displacement_ids`]
/// gives for itself.
///
/// Keyed by plugin id for the reason [`preserve_openclaw_displaced_facts`] gives:
/// a displacement's identity is the plugin it displaced, not the handle the receipt
/// chose for it.
///
/// Empty for a first enable, and that is the point — with no prior receipt there is
/// no unrecorded hand-off to recover, and a `false` on the host belongs to whoever
/// wrote it.
///
/// # Errors
///
/// Propagates a receipt-consistency error from the prior's own displaced-plugin
/// references, or an unresolvable state directory.
fn prior_unrecorded_displacements(
    prior: Option<&AdapterClaim>,
    ctx: &DriverCtx,
) -> Result<BTreeMap<String, DisplacedPlugin>, AdapterError> {
    let Some(prior) = prior_for_this_instance(prior, ctx)? else {
        return Ok(BTreeMap::new());
    };
    Ok(claim_displaced_plugins(prior)?
        .into_iter()
        .filter(|entry| !entry.applied)
        .map(|entry| (entry.plugin_id.clone(), entry))
        .collect())
}

/// Carry a prior receipt's displaced-plugin facts into its replacement — but
/// only the ownership the contract being enabled *now* still declares.
///
/// Re-enable probes the host again, and by then the plugin is disabled *by
/// this adapter* — the probe reads a positive `false` and would claim nothing,
/// losing the fact that this adapter is what turned it off. A later disable
/// would then leave the host with the bundled plugin off and no receipt
/// recording why.
///
/// Inheriting *everything* is just as wrong, and in the same direction: the
/// prior receipt records what an older version of the component claimed, so a
/// contract upgrade that dropped the declaration, replaced the plugin, or moved
/// it to another slot would be undone right here — the probe contributes
/// nothing, this hook restores the stale fact, same-home cleanup has nothing to
/// release, and `apply_enable` disables the old plugin again. The new contract
/// would never take effect. So only a plugin id the current
/// [`DisplacedPluginSpec`] list still names is carried over, and its slot is
/// taken from that declaration rather than from history;
/// [`OpenClawDriver::cleanup_replaced_claim`] hands back the rest while the
/// prior receipt is still durable.
fn preserve_openclaw_displaced_facts(
    prior: &AdapterClaim,
    next: &mut AdapterClaim,
    ctx: &DriverCtx,
) -> Result<(), AdapterError> {
    let prior_entries = match &prior.driver_payload {
        DriverPayload::OpenClaw(payload) => payload.displaced_plugins.clone(),
        _ => return Ok(()),
    };
    if prior_entries.is_empty() {
        return Ok(());
    }
    // Ownership does not travel across OpenClaw state directories. A different
    // `OPENCLAW_STATE_DIR` is a different registry, and the prior receipt says
    // nothing about who disabled a plugin *there*: in the old home this adapter
    // turned `memory-core` off, while in the new one the operator may have turned
    // it off themselves. `prepare_enable` correctly declines to claim that, and
    // inheriting here would claim it anyway — `cleanup_replaced_claim` then
    // restores the old instance while `apply_enable` disables the new one, so a
    // later disable re-enables a plugin the operator had closed. The receipt
    // already records which directory it was written against
    // (`OpenClawClaim.state_dir_resource`), so this needs no new field; the
    // cross-home branch of `cleanup_replaced_claim` runs a full `disable` on the
    // prior receipt, which hands its displacements back where they were taken,
    // and ownership in the new home comes from the new home's own probe.
    //
    // Which migrations can actually reach this branch is the Manager's decision,
    // not this driver's: `openclaw_allowed_roots` admits only the current
    // resolver's directory and the legacy one, and `validate_with_trust` rejects
    // the prior receipt before any driver hook runs. So the reachable case is a
    // legacy-resolver receipt re-enabled under an explicit `OPENCLAW_STATE_DIR`,
    // not an arbitrary A->B move — that one fails closed at claim validation,
    // which `a_receipt_in_an_unrelated_state_directory_is_rejected_not_migrated`
    // pins. Widening it would mean trusting a state-file value as an external
    // root, which is a trust-model change rather than a driver fix.
    if claim_state_dir(prior)? != require_home(ctx)? {
        return Ok(());
    }
    // Resolve every prior reference up front, and fail closed on one that does
    // not resolve. This runs before any mutation, so rejecting costs nothing,
    // while inheriting a reference whose plugin id we cannot name would hand the
    // replacement receipt control of an arbitrary framework plugin.
    let resolved = claim_displaced_plugins(prior)?;
    if !matches!(next.driver_payload, DriverPayload::OpenClaw(_)) {
        return Err(invalid_displaced_claim(
            next,
            "receipt payload is not OpenClaw",
        ));
    }
    // Keyed by *plugin id*, not by resource id. A displacement's identity is the
    // plugin it displaced: the resource id is only a handle the receipt chose for
    // it, and validation accepts any unique, correctly-referencing one. Keying on
    // the handle made this hook disagree with both of its neighbours —
    // `prepare_enable` decides attribution from `inherited_displacement_ids`, which
    // is a set of plugin ids, and `claim_displaced_plugins` rejects one plugin
    // claimed twice, also by id. A receipt whose resource had been renamed
    // consistently (which validation permits, and which `status` and `disable`
    // consume normally) then slipped past this check: `prepare_enable` wrote the
    // canonical resource for the plugin, this hook added the prior's renamed one
    // beside it, and the re-enable died in `cleanup_replaced_claim` on a duplicate
    // claim — a permanent block on re-enabling a receipt we otherwise treat as
    // valid. `dropped_displaced_plugins` asks the same question about the same
    // receipt and has always keyed it by id.
    let already_claimed: HashSet<String> = claim_displaced_plugin_ids(next)?.into_iter().collect();
    let mut additions: Vec<(ClaimResource, DisplacedPluginRef)> = Vec::new();
    for (entry, resolved) in prior_entries.iter().zip(&resolved) {
        if already_claimed.contains(&resolved.plugin_id) {
            // This enable probed the plugin again and wrote its own entry for it,
            // so there is nothing to add. Whether that entry may
            // claim the hand-off has already been performed is *not* decided here:
            // `prepare_enable` settled it when it wrote the entry, because it is
            // the only scope holding both the prior receipt and this round's probe
            // attribution. This hook receives the receipt but not `PreparedEnable`,
            // and the receipt deliberately carries no transient attribution, so
            // anything merged here could key only on the resource match — which
            // gets both directions wrong. See `carries_prior_handoff`.
            continue;
        }
        // An entry whose hand-off never ran is not ownership, so there is nothing
        // to carry — and carrying it is not neutral. The replacement receipt would
        // then hold a displacement no probe of this host produced, which is exactly
        // the entry `apply_displacements` cannot re-confirm: not in
        // `freshly_claimed`, so it gets marked applied and "disabled" by a command
        // that changes nothing. `dropped_displaced_plugins` picks it up instead,
        // `cleanup_replaced_claim` reports it as never performed, and this enable's
        // own probe decides whether to claim the plugin afresh.
        //
        // "Never ran" is read off the receipt alone here, and that is correct only
        // because the host has already been asked. An unapplied entry whose hand-off
        // the host *does* attribute to this adapter — the unreadable slot probe, the
        // process killed before the save — is recovered in `prepare_enable`, which
        // holds the prior receipt and this round's probe together and can therefore
        // tell that state apart from an operator's own disable. Such an entry arrives
        // here already claimed afresh, so the id match above took it; what reaches
        // this branch is an entry no evidence anywhere supports, and handing it back
        // through `dropped_displaced_plugins` is what keeps the plugin from being
        // stranded by a cleanup that has nothing left to attribute it to.
        if !entry.applied {
            continue;
        }
        let Some(spec) = ctx
            .declared_displaces
            .iter()
            .find(|spec| spec.id == resolved.plugin_id)
        else {
            // The contract being enabled no longer declares this plugin;
            // `cleanup_replaced_claim` restores it.
            continue;
        };
        let Some(resource) = prior.resource(&entry.resource).cloned() else {
            continue;
        };
        if next.resource(&resource.id).is_some() {
            return Err(invalid_displaced_claim(
                next,
                &format!(
                    "prior displaced plugin resource id '{}' collides with the replacement \
                     receipt",
                    resource.id
                ),
            ));
        }
        additions.push((
            resource,
            DisplacedPluginRef {
                resource: entry.resource.clone(),
                // The slot is contract metadata, not history: a declaration that
                // moved the plugin to a different exclusive slot must restore it
                // to the slot the current contract names.
                slot: spec.slot.clone(),
                // Whether the hand-off ran *is* history, and the only honest
                // source for it is the receipt that recorded it. Inheriting an
                // unapplied entry as applied would claim a transition no enable
                // ever performed; inheriting an applied one as unapplied would
                // strand a plugin this adapter really did disable.
                applied: entry.applied,
            },
        ));
    }
    for (resource, _) in &additions {
        next.resources.push(resource.clone());
    }
    let DriverPayload::OpenClaw(next_payload) = &mut next.driver_payload else {
        return Err(invalid_displaced_claim(
            next,
            "receipt payload is not OpenClaw",
        ));
    };
    for (_, entry) in additions {
        next_payload.displaced_plugins.push(entry);
    }
    Ok(())
}

/// The displaced plugins a prior receipt claims that its replacement does not,
/// both sides resolved and validated. These are exactly the entries
/// [`preserve_openclaw_displaced_facts`] declined to inherit, and
/// `cleanup_replaced_claim` hands them back before the prior receipt stops being
/// the durable record of why they are disabled.
fn dropped_displaced_plugins(
    prior: &AdapterClaim,
    next: &AdapterClaim,
) -> Result<Vec<DisplacedPlugin>, AdapterError> {
    let still_claimed: HashSet<String> = claim_displaced_plugin_ids(next)?.into_iter().collect();
    Ok(claim_displaced_plugins(prior)?
        .into_iter()
        .filter(|entry| !still_claimed.contains(&entry.plugin_id))
        .collect())
}

/// The error for a contract that declares a displaced plugin this host's
/// inventory does not have.
///
/// Reported as invalid *input* rather than a framework failure: the host
/// answered `plugins list` perfectly well, and what it said is that the
/// declaration does not match this OpenClaw. Nothing about retrying changes that.
fn missing_displacement_target(ctx: &DriverCtx, plugin_id: &str) -> AdapterError {
    AdapterError::InvalidAdapterInput {
        component: ctx.component.clone(),
        framework: ctx.framework.clone(),
        reason: format!(
            "displaced plugin '{plugin_id}' is not in this host's `openclaw plugins list` \
             inventory, so there is nothing to hand the tool names over from; check the \
             [[adapters.openclaw.displaces]] id against this OpenClaw version"
        ),
    }
}

/// The error [`OpenClawDriver::recovered_prior_handoff`] raises when the host will
/// not answer the attribution question.
///
/// [`AdapterError::FrameworkCli`] rather than a verdict, because the caller is being
/// asked to choose between two destructive answers without the evidence to choose
/// with. The reason says what is being protected and that nothing was changed, so an
/// operator can tell a transient host failure from a broken contract and knows a
/// retry is safe.
fn unreadable_handoff_evidence(ctx: &DriverCtx, plugin_id: &str) -> AdapterError {
    AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: format!(
            "cannot read the evidence that would say who disabled displaced plugin \
             '{plugin_id}' while re-enabling {}/{}: an earlier receipt of this adapter \
             names it without having marked the hand-off, and neither `config get \
             plugins.slots.<slot>` nor `config get plugins.entries.{plugin_id}.enabled` \
             answered. Nothing was changed and that receipt is kept as it stands; retry \
             once the host answers, because guessing here would either claim a disable you \
             made yourself or drop the only record of one this adapter made",
            ctx.component, ctx.framework
        ),
    }
}

/// The error [`OpenClawDriver::recovered_disable_attribution`] raises when the host
/// will not answer an attribution the `disable` it stopped is about to destroy.
///
/// [`unreadable_handoff_evidence`]'s counterpart on the other side of the same
/// question, and the two differ in what they say a guess would have cost because the
/// operations do different things next. A re-enable would leave the entry out of the
/// receipt it is writing; a disable goes on to uninstall this adapter's own plugin —
/// which resets the very slot the attribution reads — and then removes the receipt
/// over a cleanup it reports as complete. So this one has to say that the evidence is
/// still there *because nothing ran*: an operator reading it needs to know the retry
/// is a recovery rather than a cleanup, and that it has to happen before the disable
/// does anything at all.
fn unreadable_handoff_evidence_on_disable(ctx: &DriverCtx, plugin_id: &str) -> AdapterError {
    AdapterError::FrameworkCli {
        program: openclaw_bin(),
        reason: format!(
            "cannot read the evidence that would say who disabled displaced plugin \
             '{plugin_id}' while disabling {}/{}: an earlier receipt of this adapter \
             names it without having marked the hand-off, and neither `config get \
             plugins.slots.<slot>` nor `config get plugins.entries.{plugin_id}.enabled` \
             answered. Nothing was uninstalled, no slot was reset and that receipt is \
             kept exactly as it stands, so the evidence this needs is all still there; \
             retry once the host answers, because guessing would either claim a disable \
             you made yourself or uninstall this adapter's plugin over one it made — \
             which resets the slot and, once the cleanup reports complete, removes the \
             only receipt that recorded it",
            ctx.component, ctx.framework
        ),
    }
}

/// Record that the framework command for one displacement has been issued.
///
/// The counterpart of [`release_displacement_claim`]: that one removes an
/// ownership this enable must not claim, this one confirms one it is about to
/// take. Both exist because the receipt is written before the hand-off runs, and
/// a receipt that cannot tell those two states apart will eventually be asked to
/// restore a plugin nobody disabled.
///
/// # Errors
///
/// [`AdapterError::BundleInvalid`] when the receipt is not an OpenClaw one or no
/// entry names `resource_id` — both mean the caller's view of the receipt has
/// diverged from the receipt itself.
fn mark_displacement_applied(
    claim: &mut AdapterClaim,
    resource_id: &str,
) -> Result<(), AdapterError> {
    let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload else {
        return Err(invalid_displaced_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    };
    let Some(entry) = payload
        .displaced_plugins
        .iter_mut()
        .find(|entry| entry.resource == resource_id)
    else {
        return Err(invalid_displaced_claim(
            claim,
            &format!("displaced plugin resource '{resource_id}' is missing"),
        ));
    };
    entry.applied = true;
    Ok(())
}

/// Drop one displacement claim from a receipt: the payload reference and the
/// resource it points at, together.
///
/// Both or neither. Leaving the reference behind would point at a resource that
/// no longer exists, and leaving the resource behind would keep a
/// displaced-purpose entry nothing refers to — either way `claim_displaced_plugins`
/// would reject the receipt it was handed, and `status` and `disable` with it.
///
/// The id is the one the receipt names ([`DisplacedPlugin::resource`]), never one
/// re-derived from the plugin id — see that field for why the two can differ.
fn release_displacement_claim(
    claim: &mut AdapterClaim,
    resource_id: &str,
) -> Result<(), AdapterError> {
    if !matches!(claim.driver_payload, DriverPayload::OpenClaw(_)) {
        return Err(invalid_displaced_claim(
            claim,
            "receipt payload is not OpenClaw",
        ));
    }
    if let DriverPayload::OpenClaw(payload) = &mut claim.driver_payload {
        payload
            .displaced_plugins
            .retain(|entry| entry.resource != resource_id);
    }
    claim
        .resources
        .retain(|resource| resource.id != resource_id);
    Ok(())
}

fn invalid_displaced_claim(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::BundleInvalid {
        root: claim.resource_root.clone(),
        reason: format!("invalid OpenClaw displaced-plugin receipt: {reason}"),
    }
}

/// Extract skill names from a claim's `skill_resources` by parsing the
/// resource ids. Each id has the form `openclaw_skill_<name>`, and we
/// extract `<name>` as the directory name under `<home>/skills/`.
fn claim_skill_resources(claim: &AdapterClaim) -> Vec<String> {
    let payload = match &claim.driver_payload {
        DriverPayload::OpenClaw(oc) => oc,
        _ => return Vec::new(),
    };
    payload
        .skill_resources
        .iter()
        .filter_map(|id| id.strip_prefix("openclaw_skill_"))
        .map(str::to_string)
        .collect()
}

/// Count confirmed and uncertain OpenClaw config facts.
fn claim_config_counts(claim: &AdapterClaim) -> (usize, usize) {
    claim
        .resources
        .iter()
        .fold((0, 0), |(applied, pending), resource| {
            match &resource.kind {
                ClaimResourceKind::FrameworkConfig {
                    state: ConfigApplyState::Applied,
                    ..
                } => (applied + 1, pending),
                ClaimResourceKind::FrameworkConfig {
                    state: ConfigApplyState::Pending,
                    ..
                } => (applied, pending + 1),
                _ => (applied, pending),
            }
        })
}

/// ISO 8601 UTC timestamp, second precision.
fn now_iso8601() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static OPENCLAW_BIN_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct OpenClawBinEnvGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl OpenClawBinEnvGuard {
        fn unset() -> Self {
            Self::apply(None)
        }

        fn set(value: &str) -> Self {
            Self::apply(Some(value))
        }

        fn apply(value: Option<&str>) -> Self {
            let lock = OPENCLAW_BIN_ENV_LOCK.lock().expect("openclaw env lock");
            let previous = std::env::var_os("OPENCLAW_BIN");
            // SAFETY: these tests serialize every OPENCLAW_BIN mutation and
            // every command-builder read behind the same process-wide lock.
            unsafe {
                if let Some(value) = value {
                    std::env::set_var("OPENCLAW_BIN", value);
                } else {
                    std::env::remove_var("OPENCLAW_BIN");
                }
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for OpenClawBinEnvGuard {
        fn drop(&mut self) {
            // SAFETY: the lock is still held while restoring the process
            // environment, so no sibling OpenClaw test can observe a partial
            // OPENCLAW_BIN transition.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var("OPENCLAW_BIN", previous);
                } else {
                    std::env::remove_var("OPENCLAW_BIN");
                }
            }
        }
    }

    /// Doc comments on private items are checked by nothing. `missing_docs` does not
    /// apply, and `cargo doc -D warnings` stays silent because two adjacent `///`
    /// blocks with no blank line between them are perfectly legal Rust — they simply
    /// render as one run-on attached to whichever item follows. Three review rounds
    /// have now found a helper documenting its neighbour's behaviour instead of its
    /// own (a skill-name parser on a receipt struct, a disable-plan builder on a
    /// validator, and the two pairs below), so the attachment is asserted against the
    /// source directly.
    #[test]
    fn private_helper_docs_stay_attached_to_their_own_function() {
        let source = include_str!("openclaw.rs");
        let lines: Vec<&str> = source.lines().collect();

        /// The contiguous `///` block immediately above `fn <name>(`, joined.
        fn doc_above(lines: &[&str], name: &str) -> String {
            let needle = format!("fn {name}(");
            let at = lines
                .iter()
                .position(|line| line.contains(&needle))
                .unwrap_or_else(|| panic!("`{needle}` not found in openclaw.rs"));
            let mut doc = Vec::new();
            let mut i = at;
            while i > 0 {
                let prev = lines[i - 1].trim();
                if !prev.starts_with("///") {
                    break;
                }
                doc.push(prev);
                i -= 1;
            }
            doc.reverse();
            doc.join("\n")
        }

        for (name, must_own, must_not_own) in [
            // Parsing the three `plugins list` output shapes and stripping ANSI is
            // what `list_contains_plugin` does; `inventory_omits_plugin` only
            // decides whether a readable answer omits the id.
            (
                "list_contains_plugin",
                "three output shapes",
                "positively omits",
            ),
            (
                "inventory_omits_plugin",
                "positively omits",
                "three output shapes",
            ),
            // Removal semantics belong to the remover. `mark_displacement_applied`
            // drops nothing, so a doc telling the reader "both or neither" describes
            // an operation that function never performs.
            (
                "release_displacement_claim",
                "Drop one displacement claim",
                "has been issued",
            ),
            (
                "mark_displacement_applied",
                "has been issued",
                "Drop one displacement claim",
            ),
            // The two halves of the attribution question, and the pair most at risk
            // of the failure this test exists for: the long "both halves are
            // required" argument was written for the boolean and moved up to the
            // tri-state when the re-enable needed the third answer. It belongs to
            // `handoff_attribution`, which is the function that asks both halves;
            // the wrapper below it only says why collapsing the tri-state is safe
            // for a caller whose negative direction is inaction.
            (
                "handoff_attribution",
                "Both halves are required",
                "collapsed to its positive answer",
            ),
            (
                "handoff_performed_by_slot_selection",
                "collapsed to its positive answer",
                "Both halves are required",
            ),
        ] {
            let doc = doc_above(&lines, name);
            assert!(
                !doc.is_empty(),
                "`{name}` has lost its doc comment entirely — whatever sits above it \
                 now describes a different function"
            );
            assert!(
                doc.contains(must_own),
                "`{name}` must keep its own documentation ({must_own:?}):\n{doc}"
            );
            assert!(
                !doc.contains(must_not_own),
                "`{name}` has absorbed a neighbouring function's documentation \
                 ({must_not_own:?}):\n{doc}"
            );
        }
    }

    #[test]
    fn list_contains_plugin_matches_whole_token() {
        assert!(list_contains_plugin("tokenless\nother\n", "tokenless"));
        assert!(list_contains_plugin("- tokenless (v1.2)\n", "tokenless"));
        assert!(!list_contains_plugin("tokenless-extra\n", "tokenless"));
        assert!(!list_contains_plugin("", "tokenless"));
    }

    #[test]
    fn list_contains_plugin_strips_ansi() {
        let ansi_output = "\x1b[1m\x1b[32magent-sec\x1b[0m\nother\n";
        assert!(list_contains_plugin(ansi_output, "agent-sec"));
        assert!(!list_contains_plugin(ansi_output, "not-here"));
    }

    #[test]
    fn list_contains_plugin_rich_table_no_wrap() {
        let table = "\
┏━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name               ┃ ID          ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security     │ agent-sec   │ enabled │
└────────────────────┴─────────────┴─────────┘
";
        assert!(list_contains_plugin(table, "agent-sec"));
        assert!(!list_contains_plugin(table, "not-here"));
        assert!(!list_contains_plugin(table, "agent-sec-extra"));
    }

    #[test]
    fn list_contains_plugin_rich_table_wrapped() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security  │ agent-sec-core-op │ enabled │
│                 │ enclaw-plugin     │         │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
        assert!(!list_contains_plugin(table, "agent-sec"));
    }

    #[test]
    fn list_contains_plugin_rich_table_ansi_wrapped() {
        let table = "\
\x1b[1m┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓\x1b[0m
\x1b[1m┃\x1b[0m Name            \x1b[1m┃\x1b[0m ID                \x1b[1m┃\x1b[0m Status  \x1b[1m┃\x1b[0m
\x1b[1m┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩\x1b[0m
│ Agent Security  │ agent-sec-core-op │ enabled │
│                 │ enclaw-plugin     │         │
\x1b[1m└─────────────────┴───────────────────┴─────────┘\x1b[0m
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
    }

    #[test]
    fn strip_ansi_removes_sgr_and_osc() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m"), "bold");
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m text"), "green text");
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn missing_plugin_uninstall_is_idempotent_only_for_exact_error() {
        let missing = CliOutput {
            status: Some(1),
            timed_out: false,
            stdout: String::new(),
            stderr: "\x1b[31mPlugin not found: tokenless\x1b[0m\n".to_string(),
        };
        assert!(uninstall_reports_missing_plugin(&missing, "tokenless"));

        let additional_error = CliOutput {
            stderr: "Plugin not found: tokenless\nUnable to update registry\n".to_string(),
            ..missing.clone()
        };
        assert!(!uninstall_reports_missing_plugin(
            &additional_error,
            "tokenless"
        ));

        let wrong_plugin = CliOutput {
            stderr: "Plugin not found: other-plugin\n".to_string(),
            ..missing.clone()
        };
        assert!(!uninstall_reports_missing_plugin(
            &wrong_plugin,
            "tokenless"
        ));

        let timed_out = CliOutput {
            timed_out: true,
            ..missing
        };
        assert!(!uninstall_reports_missing_plugin(&timed_out, "tokenless"));
    }

    #[test]
    fn untracked_uninstall_requires_exact_plugin_and_completed_output() {
        let output = CliOutput {
            status: Some(1),
            timed_out: false,
            stdout: String::new(),
            stderr: "\x1b[31mPlugin \"tokenless\" is not associated with a tracked package install.\x1b[0m\n".to_string(),
        };
        assert!(uninstall_reports_untracked_plugin(&output, "tokenless"));
        assert!(!uninstall_reports_missing_plugin(&output, "tokenless"));
        assert!(!uninstall_reports_untracked_plugin(&output, "token"));
        for stderr in [
            "Plugin \"tokenless-other\" is not associated with a tracked package install.",
            "Plugin \"tokenless\" is not associated with a tracked package installer.",
            "Plugin \"tokenless\" is not associated with a tracked package install.\nUnable to update registry",
        ] {
            assert!(!uninstall_reports_untracked_plugin(
                &CliOutput {
                    stderr: stderr.to_string(),
                    ..output.clone()
                },
                "tokenless"
            ));
        }
        assert!(!uninstall_reports_untracked_plugin(
            &CliOutput {
                timed_out: true,
                ..output
            },
            "tokenless"
        ));
    }

    #[test]
    fn json_absence_requires_success_and_usable_diagnostics() {
        let output = CliOutput {
            status: Some(0),
            timed_out: false,
            stdout:
                r#"{"plugins":[],"diagnostics":[{"level":"info"}],"registry":{"diagnostics":[]}}"#
                    .to_string(),
            stderr: String::new(),
        };
        assert!(json_list_confirms_plugin_absent(&output, "tokenless"));
        for failed in [
            CliOutput {
                timed_out: true,
                ..output.clone()
            },
            CliOutput {
                status: Some(1),
                ..output.clone()
            },
            CliOutput {
                status: None,
                ..output.clone()
            },
            CliOutput {
                stderr: "discovery failed".to_string(),
                ..output.clone()
            },
        ] {
            assert!(!json_list_confirms_plugin_absent(&failed, "tokenless"));
        }
        for stdout in [
            r#"{"plugins":[]}"#,
            r#"{"plugins":[],"diagnostics":null}"#,
            r#"{"plugins":[],"diagnostics":[{}]}"#,
            r#"{"plugins":[],"diagnostics":[],"registry":{}}"#,
            r#"{"plugins":[{"id":""}],"diagnostics":[]}"#,
            r#"{"plugins":[{"id":"tokenless","status":"error"}],"diagnostics":[]}"#,
        ] {
            assert!(
                !json_list_confirms_plugin_absent(
                    &CliOutput {
                        stdout: stdout.to_string(),
                        ..output.clone()
                    },
                    "tokenless"
                ),
                "{stdout}"
            );
        }
    }

    #[test]
    fn is_border_line_identifies_borders() {
        assert!(is_border_line("┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓"));
        assert!(is_border_line("├──────┼──────────┤"));
        assert!(is_border_line("└──────┴──────────┘"));
        assert!(!is_border_line("│ agent-sec │ enabled │"));
        assert!(!is_border_line("plain text"));
        assert!(!is_border_line(""));
    }

    #[test]
    fn extract_cells_splits_data_line() {
        let cells = extract_cells("│ agent-sec   │ enabled │").unwrap();
        assert_eq!(cells, vec!["agent-sec", "enabled"]);
    }

    #[test]
    fn extract_cells_returns_none_for_plain_text() {
        assert!(extract_cells("plain text").is_none());
    }

    #[test]
    fn list_contains_plugin_rich_table_name_and_id_both_wrap() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Agent Security  │ agent-sec-core-op │ enabled │
│ Core Plugin     │ enclaw-plugin     │         │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(list_contains_plugin(
            table,
            "agent-sec-core-openclaw-plugin"
        ));
        assert!(!list_contains_plugin(table, "agent-sec-core-op"));
    }

    #[test]
    fn list_contains_plugin_no_false_positive_across_rows() {
        let table = "\
┏━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━┓
┃ Name            ┃ ID                ┃ Status  ┃
┡━━━━━━━━━━━━━━━━━╇━━━━━━━━━━━━━━━━━━━╇━━━━━━━━━┩
│ Plugin A        │ agent-sec-core-op │ enabled │
│ Plugin B        │ enclaw-plugin     │ enabled │
└─────────────────┴───────────────────┴─────────┘
";
        assert!(
            !list_contains_plugin(table, "agent-sec-core-openclaw-plugin"),
            "must not merge IDs from independent rows into a false match"
        );
        assert!(list_contains_plugin(table, "agent-sec-core-op"));
        assert!(list_contains_plugin(table, "enclaw-plugin"));
    }

    /// Build a host profile with the given install capabilities; version
    /// parsed from `"2026.4.14"` and inspect `--json`/`--runtime` supported by
    /// default (the fields `build_install_cmd` does not read).
    fn profile(force: bool, unsafe_install: bool) -> OpenClawHostProfile {
        OpenClawHostProfile {
            version: OpenClawVersion::parse("2026.4.14"),
            version_display: "2026.4.14".to_string(),
            supports_install_force: force,
            supports_accept_capabilities: false,
            supports_enable_accept_capabilities: false,
            unsafe_install_support: if unsafe_install {
                UnsafeInstallSupport::Effective
            } else {
                UnsafeInstallSupport::Unsupported
            },
            supports_inspect_json: true,
            supports_inspect_runtime: true,
        }
    }

    #[test]
    fn install_cmd_default_uses_force_only_no_unsafe() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, true),
            false,
        )
        .expect("force-capable host builds an install command");
        assert_eq!(cmd.program, "openclaw");
        assert_eq!(
            cmd.args,
            vec![
                "plugins",
                "install",
                "/data/adapters/tokenless/openclaw",
                "--force",
            ],
            "a normal install must never carry the unsafe flag"
        );
        assert!(cmd.env_remove.contains(&"OPENCLAW_HOME".to_string()));
        assert_eq!(
            cmd.env_set,
            vec![(
                "OPENCLAW_STATE_DIR".to_string(),
                "/home/u/.openclaw".to_string()
            )]
        );
        assert_eq!(cmd.path_prepend[0], PathBuf::from("/home/u/.local/bin"));
    }

    #[test]
    fn install_cmd_missing_force_capability_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(false, true),
            false,
        )
        .expect_err("no --force must fail before mutation");
        assert!(matches!(err, AdapterError::FrameworkCli { .. }));
    }

    #[test]
    fn install_cmd_authorized_unsafe_supported_appends_flag() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, true),
            true,
        )
        .expect("authorized + supported unsafe install builds a command");
        assert_eq!(
            cmd.args,
            vec![
                "plugins",
                "install",
                "/data/adapters/tokenless/openclaw",
                "--force",
                "--dangerously-force-unsafe-install",
            ],
            "a single install argv carries the unsafe flag exactly once"
        );
    }

    #[test]
    fn install_cmd_authorized_unsafe_unsupported_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &profile(true, false),
            true,
        )
        .expect_err("authorized but unsupported unsafe must fail before mutation");
        assert!(matches!(err, AdapterError::FrameworkCli { .. }));
    }

    #[test]
    fn install_cmd_authorized_unsafe_deprecated_noop_fails() {
        let _env = OpenClawBinEnvGuard::unset();
        let mut host = profile(true, false);
        host.unsafe_install_support = UnsafeInstallSupport::DeprecatedNoOp;
        let err = build_install_cmd(
            Path::new("/data/adapters/tokenless/openclaw"),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            &host,
            true,
        )
        .expect_err("a deprecated no-op must not be treated as an unsafe bypass");
        match err {
            AdapterError::FrameworkCli { reason, .. } => {
                assert!(reason.contains("deprecated no-op"), "{reason}");
                assert!(reason.contains("security.installPolicy"), "{reason}");
            }
            other => panic!("expected FrameworkCli, got {other:?}"),
        }
    }

    #[test]
    fn inspect_cmd_uses_runtime_when_supported() {
        let _env = OpenClawBinEnvGuard::unset();
        let with_runtime = build_inspect_cmd(
            "agent-sec",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            true,
        );
        assert_eq!(
            with_runtime.args,
            vec!["plugins", "inspect", "agent-sec", "--runtime", "--json"]
        );
        let without_runtime = build_inspect_cmd(
            "agent-sec",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
            false,
        );
        assert_eq!(
            without_runtime.args,
            vec!["plugins", "inspect", "agent-sec", "--json"]
        );
    }

    // -- version parsing / comparison ------------------------------------

    #[test]
    fn version_parses_core_and_variants() {
        let base = OpenClawVersion::parse("2026.4.14").expect("core");
        assert_eq!(base.core, [2026, 4, 14]);
        assert_eq!(base.suffix, VersionSuffix::Release);

        // Build metadata is ignored for identity.
        let build = OpenClawVersion::parse("2026.4.14+build.5").expect("build meta");
        assert_eq!(build, base);

        // Alphabetic prerelease.
        let beta = OpenClawVersion::parse("2026.4.14-beta.1").expect("beta");
        assert!(matches!(beta.suffix, VersionSuffix::Prerelease(_)));

        // Numeric correction.
        let corr = OpenClawVersion::parse("2026.5.3-1").expect("correction");
        assert_eq!(corr.suffix, VersionSuffix::Correction(vec![1]));

        // Two-component core pads the patch to zero.
        let short = OpenClawVersion::parse("2026.4").expect("short");
        assert_eq!(short.core, [2026, 4, 0]);

        // Non-numeric core is rejected.
        assert!(OpenClawVersion::parse("not.a.version").is_none());
        assert!(OpenClawVersion::parse("").is_none());
    }

    #[test]
    fn version_rejects_malformed_suffixes() {
        // An empty `-suffix` must not be treated as a plain release.
        assert!(OpenClawVersion::parse("2026.4.14-").is_none());
        // Empty build metadata must be rejected, not silently dropped.
        assert!(OpenClawVersion::parse("2026.4.14+").is_none());
        // Empty prerelease/build identifiers (double/leading/trailing dots).
        assert!(OpenClawVersion::parse("2026.4.14-beta..1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14-.1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14+build.").is_none());
        // Illegal characters in a prerelease identifier are rejected, not
        // accepted as free text.
        assert!(OpenClawVersion::parse("2026.4.14-beta_1").is_none());
        assert!(OpenClawVersion::parse("2026.4.14-beta!1").is_none());
        // Well-formed suffixes still parse (incl. hyphen inside identifiers
        // and validated-then-dropped build metadata).
        assert!(OpenClawVersion::parse("2026.4.14-rc-1").is_some());
        assert_eq!(
            OpenClawVersion::parse("2026.4.14-beta.1+build.5"),
            OpenClawVersion::parse("2026.4.14-beta.1")
        );
    }

    #[test]
    fn version_ordering_ranks_correction_above_and_prerelease_below() {
        let base = OpenClawVersion::parse("2026.5.3").unwrap();
        let corr = OpenClawVersion::parse("2026.5.3-1").unwrap();
        let beta = OpenClawVersion::parse("2026.5.3-beta.1").unwrap();
        let rc = OpenClawVersion::parse("2026.5.3-rc.2").unwrap();

        // The key departure from stock semver: numeric correction > base.
        assert!(corr > base, "numeric correction must sort above the base");
        assert!(
            beta < base,
            "alphabetic prerelease must sort below the base"
        );
        assert!(beta < rc, "beta precedes rc");
        assert!(rc < base, "prerelease precedes the release");

        // Core precedence still dominates the suffix.
        let newer = OpenClawVersion::parse("2026.5.4").unwrap();
        assert!(newer > corr);
        assert!(newer > base);

        // A larger correction number sorts above a smaller one.
        let corr2 = OpenClawVersion::parse("2026.5.3-2").unwrap();
        assert!(corr2 > corr);
    }

    #[test]
    fn version_req_satisfaction_covers_operators_and_correction() {
        let v = OpenClawVersion::parse("2026.4.24").unwrap();
        assert_eq!(openclaw_version_req_satisfied(">=2026.4.14", &v), Ok(true));
        assert_eq!(openclaw_version_req_satisfied(">=2026.4.24", &v), Ok(true));
        assert_eq!(openclaw_version_req_satisfied(">=2026.5.0", &v), Ok(false));
        assert_eq!(openclaw_version_req_satisfied(">2026.4.24", &v), Ok(false));
        assert_eq!(openclaw_version_req_satisfied("<2026.5.0", &v), Ok(true));
        assert_eq!(
            openclaw_version_req_satisfied(">=2026.4.14, <2026.5.0", &v),
            Ok(true)
        );
        // Bare version behaves as a minimum.
        assert_eq!(openclaw_version_req_satisfied("2026.4.14", &v), Ok(true));

        // A numeric-correction host satisfies a `>=` on the base release.
        let corr = OpenClawVersion::parse("2026.5.3-1").unwrap();
        assert_eq!(
            openclaw_version_req_satisfied(">=2026.5.3", &corr),
            Ok(true)
        );

        // Malformed requirement is an error, not a silent false.
        assert!(openclaw_version_req_satisfied(">=not.a.version", &v).is_err());
        assert!(
            openclaw_version_req_satisfied(">=2027.0.0, >=not.a.version", &v).is_err(),
            "every clause must be validated before a non-match is returned"
        );
        assert!(openclaw_version_req_satisfied("", &v).is_err());
        // A malformed suffix in the constraint is an error, not a match.
        assert!(openclaw_version_req_satisfied(">=2026.4.14-", &v).is_err());
        // Empty clauses (double/leading/trailing comma) are errors.
        assert!(openclaw_version_req_satisfied(">=2026.4.14,,<2027.0.0", &v).is_err());
        assert!(openclaw_version_req_satisfied(">=2026.4.14,", &v).is_err());
        assert!(openclaw_version_req_satisfied(",>=2026.4.14", &v).is_err());
    }

    #[test]
    fn version_output_parsing_extracts_token() {
        assert_eq!(
            parse_openclaw_version_output("openclaw 2026.4.14"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output("OpenClaw CLI version v2026.4.14 (abcdef)"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output("2026.5.3-1\n"),
            OpenClawVersion::parse("2026.5.3-1")
        );
        assert!(parse_openclaw_version_output("no version here").is_none());
    }

    #[test]
    fn version_output_only_accepts_calendar_shape() {
        // An unrelated dependency/runtime version must not be mistaken for
        // OpenClaw's own version, and a non-calendar token is ignored.
        assert!(
            parse_openclaw_version_output(
                "warning: node 22.14.0 is unsupported\nopenclaw nightly-build"
            )
            .is_none(),
            "22.14.0 is not calendar-shaped and nightly-build is not a version"
        );
        // A leading warning number does not derail extraction of the real one.
        assert_eq!(
            parse_openclaw_version_output("note: 3 plugins loaded\nopenclaw 2026.4.14"),
            OpenClawVersion::parse("2026.4.14")
        );
        assert_eq!(
            parse_openclaw_version_output(
                "warning: certificate expires on 2099.1.1\nopenclaw 2026.4.14"
            ),
            OpenClawVersion::parse("2026.4.14"),
            "a calendar-shaped warning token must not outrank the explicit version line"
        );
        assert!(
            parse_openclaw_version_output("warning: retry after 2099.1.1").is_none(),
            "a diagnostic date is not an OpenClaw version"
        );
        assert!(
            parse_openclaw_version_output("openclaw 2026.4.14\n2026.5.0").is_none(),
            "multiple plausible version lines are ambiguous"
        );
        // A two-component version is not accepted as a host version.
        assert!(parse_openclaw_version_output("openclaw 2026.4").is_none());
    }

    #[test]
    fn help_lists_flag_requires_whole_token() {
        assert!(help_lists_flag("  --force    overwrite", "--force"));
        assert!(help_lists_flag("--json=<path>  machine readable", "--json"));
        assert!(help_lists_flag("use --json, or --yaml", "--json"));
        assert!(help_lists_flag(
            "  --dangerously-force-unsafe-install  bypass",
            "--dangerously-force-unsafe-install"
        ));
        // Similar-prefixed flags must not be mistaken for the target flag.
        assert!(!help_lists_flag("--force-color   colorize", "--force"));
        assert!(!help_lists_flag("--json-file <p>  write json", "--json"));
        assert!(!help_lists_flag(
            "--runtime-only   skip static",
            "--runtime"
        ));
    }

    #[test]
    fn unsafe_install_help_distinguishes_effective_from_noop() {
        assert_eq!(
            unsafe_install_support(
                "  --dangerously-force-unsafe-install  bypass plugin safety checks"
            ),
            UnsafeInstallSupport::Effective
        );
        assert_eq!(
            unsafe_install_support(
                "  --dangerously-force-unsafe-install  Deprecated no-op; security.installPolicy may still block"
            ),
            UnsafeInstallSupport::DeprecatedNoOp
        );
        assert_eq!(
            unsafe_install_support("  --force  overwrite an existing plugin"),
            UnsafeInstallSupport::Unsupported
        );
    }

    #[test]
    fn extract_trailing_json_tolerates_leading_diagnostics() {
        let clean = r#"{"plugin":{"status":"loaded"}}"#;
        assert_eq!(
            extract_trailing_json(clean).and_then(|v| v
                .get("plugin")?
                .get("status")?
                .as_str()
                .map(str::to_string)),
            Some("loaded".to_string())
        );

        let with_diag = "warning: legacy diagnostic line\nreading registry...\n{\"plugin\":{\"status\":\"loaded\"}}\n";
        let value = extract_trailing_json(with_diag).expect("json after diagnostics");
        assert_eq!(
            value
                .get("plugin")
                .and_then(|p| p.get("status"))
                .and_then(|s| s.as_str()),
            Some("loaded")
        );

        assert!(extract_trailing_json("not json at all").is_none());
        assert!(extract_trailing_json("").is_none());
    }

    #[test]
    fn uninstall_cmd_uses_force() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_uninstall_cmd(
            "tokenless",
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(
            cmd.args,
            vec!["plugins", "uninstall", "tokenless", "--force"]
        );
    }

    #[test]
    fn plugin_manifest_id_is_read_from_real_openclaw_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("openclaw.plugin.json"),
            br#"{"id":"tokenless","name":"Tokenless"}"#,
        )
        .expect("write manifest");

        assert_eq!(
            read_plugin_manifest_id(dir.path(), "openclaw.plugin.json").expect("read"),
            Some("tokenless".to_string())
        );
    }

    #[test]
    fn summarize_prioritizes_cleanup_failed() {
        assert_eq!(
            summarize(
                ClaimStatus::CleanupFailed,
                true,
                ConditionStatus::True,
                None
            ),
            AdapterSummary::CleanupFailed
        );
    }

    #[test]
    fn summarize_healthy_only_when_detected_and_registered() {
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::True, None),
            AdapterSummary::Healthy
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, false, ConditionStatus::True, None),
            AdapterSummary::Degraded
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::False, None),
            AdapterSummary::Degraded
        );
        assert_eq!(
            summarize(ClaimStatus::Enabled, true, ConditionStatus::Unknown, None),
            AdapterSummary::Unknown
        );
    }

    #[test]
    fn summarize_degrades_when_a_displaced_plugin_came_back() {
        // The adapter's own plugin verifies clean; the collision is what fails.
        assert_eq!(
            summarize(
                ClaimStatus::Enabled,
                true,
                ConditionStatus::True,
                Some(ConditionStatus::False)
            ),
            AdapterSummary::Degraded
        );
        assert_eq!(
            summarize(
                ClaimStatus::Enabled,
                true,
                ConditionStatus::True,
                Some(ConditionStatus::Unknown)
            ),
            AdapterSummary::Unknown
        );
        assert_eq!(
            summarize(
                ClaimStatus::Enabled,
                true,
                ConditionStatus::True,
                Some(ConditionStatus::True)
            ),
            AdapterSummary::Healthy
        );
        // CleanupFailed still outranks a displacement verdict.
        assert_eq!(
            summarize(
                ClaimStatus::CleanupFailed,
                true,
                ConditionStatus::True,
                Some(ConditionStatus::True)
            ),
            AdapterSummary::CleanupFailed
        );
    }

    // -- slot / enablement classifiers ----------------------------------

    #[test]
    fn explicit_off_slot_is_not_an_empty_slot() {
        // OpenClaw spells a deliberately closed memory slot `none`; restoring
        // the displaced plugin would re-run slot selection and undo it.
        assert_eq!(
            slot_restore_decision(Some("none"), Some("memory-anolisa"), "memory-core"),
            SlotRestore::ExplicitlyOff("none".to_string())
        );
        // Same off-words the enablement probe honors, so one host's rendering of
        // "off" classifies identically whichever key it is read from.
        for token in ["false", "0", "no", "off", "disabled"] {
            assert_eq!(
                slot_restore_decision(Some(token), Some("memory-anolisa"), "memory-core"),
                SlotRestore::ExplicitlyOff(token.to_string()),
                "'{token}' is an explicit off, not a vacant slot"
            );
        }
    }

    #[test]
    fn vacant_and_unreadable_slots_still_restore() {
        // Nothing behind the slot: restoring is the only way the host gets a
        // memory backend back.
        for token in ["", "null", "nil", "undefined", "nan", "(empty)"] {
            assert_eq!(
                slot_restore_decision(Some(token), Some("memory-anolisa"), "memory-core"),
                SlotRestore::Proceed,
                "'{token}' is not evidence of an operator's choice"
            );
        }
        // A probe the host cannot answer at all is not evidence either.
        assert_eq!(
            slot_restore_decision(None, Some("memory-anolisa"), "memory-core"),
            SlotRestore::Proceed
        );
    }

    #[test]
    fn recognized_owners_beat_sentinel_reading() {
        // This adapter's own plugin, and the displaced plugin itself, both mean
        // "restore" — even where the id happens to read like an off word.
        assert_eq!(
            slot_restore_decision(
                Some("memory-anolisa"),
                Some("memory-anolisa"),
                "memory-core"
            ),
            SlotRestore::Proceed
        );
        assert_eq!(
            slot_restore_decision(Some("memory-core"), Some("memory-anolisa"), "memory-core"),
            SlotRestore::Proceed
        );
        assert_eq!(
            slot_restore_decision(Some("none"), Some("none"), "memory-core"),
            SlotRestore::Proceed,
            "a plugin really named 'none' still holds its own slot"
        );
    }

    /// Build a CLI output whose stdout is exactly `body`.
    fn output_with(body: &str) -> CliOutput {
        CliOutput {
            status: Some(0),
            timed_out: false,
            stdout: body.to_string(),
            stderr: String::new(),
        }
    }

    fn names_in(body: &str) -> Vec<String> {
        names_in_key(body, "plugins.deny")
    }

    fn names_in_key(body: &str, key: &str) -> Vec<String> {
        match policy_id_list(&output_with(body), key) {
            PolicyIdList::Named(ids) => ids,
            PolicyIdList::Vacant => Vec::new(),
        }
    }

    /// The shape OpenClaw actually renders an array in: pretty JSON, one quoted
    /// element per line. A whole-token text search misses `"memory-core",` on the
    /// quotes and comma; a last-line reduction reads the closing `]` as vacant.
    #[test]
    fn policy_list_reads_a_pretty_json_array() {
        assert_eq!(
            names_in("[\n  \"memory-core\",\n  \"other\"\n]"),
            vec!["memory-core".to_string(), "other".to_string()]
        );
        // Compact, enveloped, and behind a preamble + key echo.
        assert_eq!(
            names_in(r#"["memory-core","other"]"#),
            vec!["memory-core".to_string(), "other".to_string()]
        );
        assert_eq!(
            names_in(r#"{"value":["memory-core"]}"#),
            vec!["memory-core".to_string()]
        );
        assert_eq!(
            names_in("reading policy...\nplugins.deny = [\n  \"memory-core\"\n]"),
            vec!["memory-core".to_string()]
        );
    }

    /// Empty and null are a host saying "nothing here", never a restriction —
    /// guessing otherwise would switch the hand-off off wherever the key is
    /// merely unset.
    #[test]
    fn policy_list_reads_empty_json_as_vacant() {
        for body in ["[]", "[\n]", "null", "  \n", ""] {
            assert_eq!(
                policy_id_list(&output_with(body), "plugins.deny"),
                PolicyIdList::Vacant,
                "'{body}' names nothing"
            );
        }
    }

    /// The reported failure: a diagnostics preamble plus a key echo whose value
    /// is null. There is no `[` or `{` for the JSON reader to anchor on, so the
    /// fallback runs — and reading every line as a value list turns the preamble's
    /// words into list entries, which `policy_blocking_plugin` then reports as a
    /// policy naming somebody else and omitting the plugin. Only the queried key's
    /// own echo may supply the value.
    #[test]
    fn policy_list_does_not_read_a_diagnostic_preamble_as_ids() {
        for key in ["plugins.allow", "plugins.deny"] {
            for body in [
                format!("reading policy...\n{key} = null"),
                format!("reading policy...\n{key} = []"),
                format!("reading policy...\n{key} ="),
                format!("reading policy...\n{key} = NULL"),
                format!("reading policy...\n{key} = unset"),
                format!("reading policy...\n{key} = -"),
            ] {
                assert_eq!(
                    policy_id_list(&output_with(&body), key),
                    PolicyIdList::Vacant,
                    "a preamble plus a vacant value names nobody: '{body}'"
                );
            }
        }
    }

    /// A sentence with a separator in it is a diagnostic, not a `key = value`
    /// echo, and its tail is prose rather than a list of ids.
    #[test]
    fn policy_list_ignores_a_foreign_key_echo() {
        assert_eq!(
            policy_id_list(&output_with("warning: config not found"), "plugins.allow"),
            PolicyIdList::Vacant
        );
        assert_eq!(
            policy_id_list(
                &output_with("note: reading plugins.allow\nplugins.allow = null"),
                "plugins.allow"
            ),
            PolicyIdList::Vacant
        );
        // The real echo still wins over prose on another line.
        assert_eq!(
            names_in_key(
                "reading policy...\nplugins.allow = memory-core",
                "plugins.allow"
            ),
            vec!["memory-core".to_string()]
        );
    }

    /// Vacant spellings are matched case-insensitively, because a token read
    /// straight out of a policy answer is not lower-cased the way one reduced by
    /// `config_answer_token` is.
    #[test]
    fn policy_list_treats_every_vacant_spelling_as_vacant() {
        for token in [
            "null",
            "NULL",
            "Null",
            "none",
            "(none)",
            "nil",
            "undefined",
            "nan",
            "(empty)",
            "unset",
            "<unset>",
            "n/a",
            "-",
        ] {
            assert_eq!(
                names_in_key(&format!("plugins.allow = {token}"), "plugins.allow"),
                Vec::<String>::new(),
                "'{token}' is a placeholder, not a plugin id"
            );
        }
    }

    /// Hosts that do not answer in JSON still have to be read: a bare value, a
    /// bare multi-line list, and this key's own `key = value` echo.
    #[test]
    fn policy_list_falls_back_to_a_line_scan() {
        assert_eq!(names_in("memory-core"), vec!["memory-core".to_string()]);
        assert_eq!(
            names_in("plugins.deny = memory-core, other"),
            vec!["memory-core".to_string(), "other".to_string()]
        );
        assert_eq!(
            names_in("memory-core\nother\n"),
            vec!["memory-core".to_string(), "other".to_string()]
        );
        assert_eq!(names_in("plugins.deny = null"), Vec::<String>::new());
        // A bare value is still read when the host echoes no key at all.
        assert_eq!(
            names_in_key("memory-core", "plugins.allow"),
            vec!["memory-core".to_string()]
        );
    }

    /// `restore_conditions_note` is the enable-side description of the branch set
    /// `restore_decision` applies, and the two live in different places. This is
    /// the drift guard the note's doc promises: every `RestoreDecision::Skip*`
    /// must be enumerated, for a slotless declaration as well as a slotful one,
    /// and neither may claim the restore is unconditional.
    #[test]
    fn restore_conditions_note_covers_every_veto() {
        for note in [
            restore_conditions_note(Some("memory")),
            restore_conditions_note(None),
        ] {
            for (needle, veto) in [
                ("inventory", "RestoreDecision::SkipAbsent"),
                (
                    "plugins.enabled",
                    "RestoreDecision::SkipPluginsGloballyDisabled",
                ),
                ("plugins.deny", "RestoreDecision::SkipDenied"),
                ("plugins.allow", "RestoreDecision::SkipAllowlistExcluded"),
            ] {
                assert!(
                    note.contains(needle),
                    "{veto} can release the ownership without consulting a slot, so the \
                     enable preview must name it: {note}"
                );
            }
            assert!(
                !note.contains("unconditional"),
                "no restore is unconditional: {note}"
            );
        }

        let slotful = restore_conditions_note(Some("memory"));
        for (needle, veto) in [
            (
                "plugins.slots.memory",
                "RestoreDecision::SkipSlotOwned / SkipSlotClosed",
            ),
            ("another plugin", "RestoreDecision::SkipSlotOwned"),
            ("explicitly closed", "RestoreDecision::SkipSlotClosed"),
        ] {
            assert!(
                slotful.contains(needle),
                "{veto} applies when a slot is declared: {slotful}"
            );
        }
        assert!(
            !restore_conditions_note(None).contains("plugins.slots."),
            "a slotless declaration has no slot to name: {}",
            restore_conditions_note(None)
        );

        // The one variant the note must *not* enumerate: it records that this
        // enable failed before its hand-off ran, which is not a condition the host
        // can reach between enable and disable. See `restore_conditions_note`.
        for note in [
            restore_conditions_note(Some("memory")),
            restore_conditions_note(None),
        ] {
            assert!(
                !note.contains("hand-off"),
                "RestoreDecision::SkipNotApplied belongs to the disable-side preview, \
                 which renders it from `restore_decision` directly; an enable preview \
                 that named it would describe a branch the operation cannot take: {note}"
            );
        }
    }

    #[test]
    fn third_plugin_keeps_the_slot() {
        assert_eq!(
            slot_restore_decision(
                Some("memory-lancedb"),
                Some("memory-anolisa"),
                "memory-core"
            ),
            SlotRestore::OwnedByThird("memory-lancedb".to_string())
        );
    }

    // -- config set cmd -------------------------------------------------

    #[test]
    fn config_set_cmd_string_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "plugins.entries.sec.enabled",
            &toml::Value::String("true".to_string()),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.program, "openclaw");
        assert_eq!(
            cmd.args,
            vec!["config", "set", "plugins.entries.sec.enabled", "true"]
        );
    }

    #[test]
    fn config_set_cmd_boolean_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "debug.enabled",
            &toml::Value::Boolean(true),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.args, vec!["config", "set", "debug.enabled", "true"]);
    }

    #[test]
    fn config_set_cmd_integer_value() {
        let _env = OpenClawBinEnvGuard::unset();
        let cmd = build_config_set_cmd(
            "limits.max_plugins",
            &toml::Value::Integer(42),
            Path::new("/home/u/.openclaw"),
            Some(Path::new("/home/u")),
        );
        assert_eq!(cmd.args, vec!["config", "set", "limits.max_plugins", "42"]);
    }

    #[test]
    fn config_value_to_cli_string_covers_types() {
        assert_eq!(
            config_value_to_cli_string(&toml::Value::String("hello".into())),
            "hello"
        );
        assert_eq!(config_value_to_cli_string(&toml::Value::Integer(7)), "7");
        assert_eq!(config_value_to_cli_string(&toml::Value::Float(2.5)), "2.5");
        assert_eq!(
            config_value_to_cli_string(&toml::Value::Boolean(false)),
            "false"
        );
    }

    // -- claim_skill_resources / claim_config_counts --------------------

    #[test]
    fn claim_skill_resources_extracts_names() {
        let claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "test".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: None,
            adapter_type: None,
            enabled_at: "2026-01-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::CleanupFailed,
            notices: Vec::new(),
            resources: vec![
                ClaimResource {
                    id: "openclaw_config_0".to_string(),
                    purpose: "openclaw_config".to_string(),
                    kind: ClaimResourceKind::FrameworkConfig {
                        framework: "openclaw".to_string(),
                        key: "applied.key".to_string(),
                        state: ConfigApplyState::Applied,
                    },
                },
                ClaimResource {
                    id: "openclaw_config_1".to_string(),
                    purpose: "openclaw_config".to_string(),
                    kind: ClaimResourceKind::FrameworkConfig {
                        framework: "openclaw".to_string(),
                        key: "pending.key".to_string(),
                        state: ConfigApplyState::Pending,
                    },
                },
            ],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: "s".to_string(),
                plugin_resource: "p".to_string(),
                skill_resources: vec![
                    "openclaw_skill_sec-audit".to_string(),
                    "openclaw_skill_cred-scan".to_string(),
                ],
                config_resources: vec!["openclaw_config_0".to_string()],

                displaced_plugins: Vec::new(),
            }),
        };
        let skills = claim_skill_resources(&claim);
        assert_eq!(skills, vec!["sec-audit", "cred-scan"]);
        assert_eq!(claim_config_counts(&claim), (1, 1));
    }

    #[test]
    fn skill_bundle_plan_and_claim_skip_plugin_registration() {
        use crate::adapter::driver::{AdapterOps, CliOutput, DeclaredSkill};

        struct StubOps;
        impl AdapterOps for StubOps {
            fn run_framework_cli(&self, _: FrameworkCommand) -> Result<CliOutput, AdapterError> {
                unimplemented!()
            }
            fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn copy_file(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
                unimplemented!()
            }
            fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn read_file(&self, _: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
                unimplemented!()
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("marker"), b"x").expect("write");
        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home"));
        let ops = StubOps;
        let ctx = DriverCtx {
            component: "os-skills".to_string(),
            framework: "openclaw".to_string(),
            layout: &layout,
            resource_root: dir.path().to_path_buf(),
            user_home: Some(PathBuf::from("/tmp/test-home")),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: Some("skill_bundle".to_string()),
            declared_skills: vec![DeclaredSkill {
                name: "install-openclaw".to_string(),
                source: Some(PathBuf::from("/usr/share/anolisa/skills/install-openclaw")),
            }],
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            declared_displaces: Vec::new(),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            dry_run: true,
            ops: &ops,
        };
        let driver = OpenClawDriver::new();
        let bundle = driver.read_bundle(&ctx).expect("read bundle");
        assert!(bundle.plugin_id.is_none());

        let plan = driver.plan_enable(&bundle, None, &ctx).expect("plan");
        assert!(plan.register_command.is_none());
        assert!(
            plan.actions
                .iter()
                .all(|action| !action.contains("register openclaw plugin")),
        );

        let (claim, _prepared) = driver.prepare_enable(&bundle, None, &ctx).expect("claim");
        assert!(claim.plugin_id.is_none());
        assert_eq!(claim.adapter_type.as_deref(), Some("skill_bundle"));
        assert!(
            claim.resources.iter().all(|resource| !matches!(
                resource.kind,
                ClaimResourceKind::FrameworkPlugin { .. }
            )),
        );
        assert_eq!(claim_skill_resources(&claim), vec!["install-openclaw"]);

        let _env = OpenClawBinEnvGuard::set("/bin/sh");
        let report = driver.status(&claim, &ctx).expect("status");

        assert_eq!(report.summary, AdapterSummary::Healthy);
        assert!(
            report
                .conditions
                .iter()
                .all(|condition| condition.kind != AdapterConditionKind::PluginRegistered),
            "skill_bundle status must not require plugin registration"
        );
        assert!(report.conditions.iter().any(|condition| {
            condition.kind == AdapterConditionKind::VerificationSupported
                && condition.status == ConditionStatus::True
        }));
    }

    /// A mismatched (or under-capable) `PreparedEnable` must fail closed in
    /// `apply_enable` **before** any framework CLI runs. The ops handle panics
    /// on `run_framework_cli`, so any test reaching this line proves no
    /// mutation was attempted.
    #[test]
    fn apply_enable_rejects_mismatched_prepared_state() {
        use crate::adapter::driver::{AdapterOps, CliOutput};

        struct PanicOps;
        impl AdapterOps for PanicOps {
            fn run_framework_cli(&self, _: FrameworkCommand) -> Result<CliOutput, AdapterError> {
                panic!("apply must fail closed before running any framework CLI");
            }
            fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                panic!("no tree copy before validation");
            }
            fn copy_file(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
                unimplemented!()
            }
            fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
                unimplemented!()
            }
            fn read_file(&self, _: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
                unimplemented!()
            }
        }

        let layout = anolisa_platform::fs_layout::FsLayout::user(PathBuf::from("/tmp/test-home"));
        let ops = PanicOps;
        let mk_ctx = |adapter_type: Option<&str>, allow_unsafe: bool| DriverCtx {
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            layout: &layout,
            resource_root: PathBuf::from("/tmp/test-home/resource"),
            user_home: Some(PathBuf::from("/tmp/test-home")),
            declared_plugin_id: None,
            requested_profiles: Vec::new(),
            adapter_type: adapter_type.map(str::to_string),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            declared_displaces: Vec::new(),
            framework_version_req: None,
            allow_unsafe_plugin_install: allow_unsafe,
            dry_run: false,
            ops: &ops,
        };
        let mut plugin_claim = AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "openclaw".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: None,
            enabled_at: "2026-01-01T00:00:00Z".to_string(),
            resource_root: PathBuf::from("/tmp/test-home/resource"),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: PURPOSE_PLUGIN.to_string(),
                kind: ClaimResourceKind::FrameworkPlugin {
                    framework: "openclaw".to_string(),
                    plugin_id: "tokenless".to_string(),
                },
            }],
            driver_payload: DriverPayload::OpenClaw(OpenClawClaim {
                state_dir_resource: RES_STATE_DIR.to_string(),
                plugin_resource: RES_PLUGIN.to_string(),
                skill_resources: Vec::new(),
                config_resources: Vec::new(),

                displaced_plugins: Vec::new(),
            }),
        };
        let mut skill_claim = plugin_claim.clone();
        skill_claim.adapter_type = Some("skill_bundle".to_string());
        skill_claim.plugin_id = None;

        let driver = OpenClawDriver::new();
        let _env = OpenClawBinEnvGuard::unset();

        // Plugin adapter but no prepared capabilities → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::None,
                &mk_ctx(None, false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Skill bundle but plugin capabilities supplied → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut skill_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: true,
                    supports_inspect_json: true,
                    supports_inspect_runtime: true,
                    selected_config_indices: Vec::new(),
                    freshly_claimed_displacements: Vec::new(),
                },
                &mk_ctx(Some("skill_bundle"), false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Plugin adapter but the host cannot produce JSON inspect → reject.
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: true,
                    supports_inspect_json: false,
                    supports_inspect_runtime: false,
                    selected_config_indices: Vec::new(),
                    freshly_claimed_displacements: Vec::new(),
                },
                &mk_ctx(None, false),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));

        // Unsafe authorized but the prepared state says the host lacks the
        // flag → reject (never add the dangerous flag on an unverified host).
        assert!(matches!(
            driver.apply_enable(
                &mut plugin_claim,
                &PreparedEnable::OpenClaw {
                    supports_accept_capabilities: false,
                    supports_enable_accept_capabilities: false,
                    supports_unsafe_install: false,
                    supports_inspect_json: true,
                    supports_inspect_runtime: false,
                    selected_config_indices: Vec::new(),
                    freshly_claimed_displacements: Vec::new(),
                },
                &mk_ctx(None, true),
                &mut (),
            ),
            Err(AdapterError::FrameworkCli { .. })
        ));
    }
}
