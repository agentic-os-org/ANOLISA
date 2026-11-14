//! Read-only configuration diagnosis using the mount loader's actual decisions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use skillfs_core::{ParseConfig, ParseStatus, SkillEntry, store::SkillStore, views::ViewsConfig};

use super::mount_file::{self, Observer};
use super::{OutputFormat, escape_for_diagnostics};

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[derive(Serialize)]
struct Report {
    schema_version: u8,
    target: &'static str,
    status: &'static str,
    config_path: String,
    mountpoint: Option<String>,
    sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocker: Option<String>,
    requested_skill: Option<String>,
    selection_status: &'static str,
    views: ViewsReport,
    skills: Vec<SkillReport>,
}

#[derive(Serialize)]
struct ViewsReport {
    path: Option<String>,
    status: &'static str,
    effective_default: Option<String>,
}

#[derive(Serialize)]
struct SkillReport {
    name: String,
    selected: CopyReport,
    shadowed: Vec<CopyReport>,
    effective_default: Membership,
    default_reason: &'static str,
    configured_views: Option<Vec<String>>,
    secondary_views: Option<Vec<String>>,
}

#[derive(Serialize)]
struct CopyReport {
    source: String,
    path: String,
    parse_status: &'static str,
    parse_message: Option<String>,
    enabled: Option<bool>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Membership {
    Known(bool),
    Unknown(&'static str),
}

struct Diagnosis {
    report: Report,
    skills: BTreeMap<String, SkillReport>,
    scan_complete: bool,
    requested_discovered: bool,
}

impl Diagnosis {
    fn new(config: &Path, skill: Option<&str>) -> Self {
        Self {
            report: Report {
                schema_version: 1,
                target: "configuration",
                status: "complete",
                config_path: config.to_string_lossy().into_owned(),
                mountpoint: None,
                sources: Vec::new(),
                phase: None,
                blocker: None,
                requested_skill: skill.map(str::to_owned),
                selection_status: "all",
                views: ViewsReport {
                    path: None,
                    status: "unknown",
                    effective_default: None,
                },
                skills: Vec::new(),
            },
            skills: BTreeMap::new(),
            scan_complete: false,
            requested_discovered: false,
        }
    }

    fn fail(&mut self, phase: &'static str, error: impl std::fmt::Display) {
        self.report.status = "incomplete";
        self.report.phase = Some(phase);
        self.report.blocker = Some(error.to_string());
    }

    fn discover(&mut self, name: &str) {
        if self.report.requested_skill.as_deref() == Some(name) {
            self.requested_discovered = true;
        }
    }

    fn inspect(&mut self, config: &Path) {
        let mount = match mount_file::load(config) {
            Ok(Some(mount)) => mount,
            Ok(None) => {
                self.fail(
                    "config",
                    "configuration does not contain mount sources and mountpoint",
                );
                return;
            }
            Err(error) => {
                self.fail("config", error);
                return;
            }
        };
        let mount = match mount.validate_observed(self) {
            Ok(mount) => mount,
            Err(error) => {
                self.fail("source_validation", error);
                return;
            }
        };
        let store = match mount_file::load_sources_observed(
            &mount.sources,
            &ParseConfig::default(),
            self,
        ) {
            Ok(store) => store,
            Err(error) => {
                self.fail("source_loading", error);
                return;
            }
        };
        self.scan_complete = true;
        // Validation rejects empty source lists; the first normalized source
        // alone owns views, just as it does for a configuration mount.
        let source = &mount.sources[0];
        match load_views(source) {
            Ok(Some(views)) => {
                self.report.views.status = "loaded";
                self.apply_views(&views, &store);
            }
            Ok(None) => {
                self.report.views.status = "absent";
                for skill in self.skills.values_mut() {
                    skill.effective_default = Membership::Known(true);
                    skill.default_reason = "no_views_configuration";
                    skill.configured_views = Some(Vec::new());
                    skill.secondary_views = Some(Vec::new());
                }
            }
            Err(error) => {
                self.report.views.status = "invalid_or_unreadable";
                self.fail("views_loading", error);
            }
        }
    }

    fn apply_views(&mut self, views: &ViewsConfig, store: &SkillStore) {
        let default = views.default_view();
        self.report.views.effective_default = default.map(|view| view.name.clone());
        let effective: HashSet<_> = views.effective_default_skills(store).into_iter().collect();
        let secondary = views.secondary_views();
        for (name, skill) in &mut self.skills {
            let configured_views: Vec<_> = views
                .views
                .iter()
                .filter(|view| view.skills.contains(name))
                .map(|view| view.name.clone())
                .collect();
            skill.effective_default = Membership::Known(effective.contains(name));
            skill.default_reason = if default.is_some_and(|view| view.skills.contains(name)) {
                "explicit"
            } else if configured_views.is_empty() {
                "unassigned"
            } else {
                "assigned_secondary"
            };
            skill.configured_views = Some(configured_views);
            skill.secondary_views = Some(
                secondary
                    .iter()
                    .filter(|view| view.skills.contains(name))
                    .map(|view| view.name.clone())
                    .collect(),
            );
        }
    }

    fn finish(mut self) -> Report {
        self.report.selection_status = match self.report.requested_skill.as_deref() {
            None => "all",
            Some(_) if self.requested_discovered => "discovered",
            Some(_) if self.scan_complete => "not_discovered",
            Some(_) => "unknown",
        };
        let source_order: HashMap<_, _> = self
            .report
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.as_str(), index))
            .collect();
        for (name, mut skill) in self.skills {
            if self
                .report
                .requested_skill
                .as_ref()
                .is_some_and(|requested| requested != &name)
            {
                continue;
            }
            skill.shadowed.sort_by(|left, right| {
                source_order
                    .get(left.source.as_str())
                    .cmp(&source_order.get(right.source.as_str()))
                    .then_with(|| left.path.cmp(&right.path))
            });
            self.report.skills.push(skill);
        }
        self.report
    }
}

impl Observer for Diagnosis {
    fn mountpoint(&mut self, path: &Path) {
        self.report.mountpoint = Some(path.to_string_lossy().into_owned());
    }

    fn source(&mut self, path: &Path) {
        if self.report.sources.is_empty() {
            self.report.views.path = Some(
                path.join("skillfs-views.toml")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        self.report
            .sources
            .push(path.to_string_lossy().into_owned());
    }

    fn selected(&mut self, source: &Path, name: &str, entry: &SkillEntry) {
        self.discover(name);
        self.skills.insert(
            name.to_owned(),
            SkillReport {
                name: name.to_owned(),
                selected: CopyReport::from_entry(source, entry),
                shadowed: Vec::new(),
                effective_default: Membership::Unknown("unknown"),
                default_reason: "unknown",
                configured_views: None,
                secondary_views: None,
            },
        );
    }

    fn shadowed(&mut self, source: &Path, name: &str, entry: &SkillEntry) {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.shadowed.push(CopyReport::from_entry(source, entry));
        }
    }

    fn shadowed_error(
        &mut self,
        source: &Path,
        name: &str,
        error: &skillfs_core::store::LoadError,
    ) {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.shadowed.push(CopyReport {
                source: source.to_string_lossy().into_owned(),
                path: error.path.to_string_lossy().into_owned(),
                parse_status: "unloadable",
                parse_message: Some(error.error.clone()),
                enabled: None,
            });
        }
    }

    fn rejected(&mut self, name: Option<&str>) {
        if let Some(name) = name {
            self.discover(name);
        }
    }
}

impl CopyReport {
    fn from_entry(source: &Path, entry: &SkillEntry) -> Self {
        let (parse_status, parse_message) = match &entry.parse_status {
            ParseStatus::Ok => ("ok", None),
            ParseStatus::Degraded(message) => ("degraded", Some(message.clone())),
            ParseStatus::Error(message) => ("error", Some(message.clone())),
        };
        Self {
            source: source.to_string_lossy().into_owned(),
            path: entry.source_path.to_string_lossy().into_owned(),
            parse_status,
            parse_message,
            enabled: Some(entry.metadata.enabled),
        }
    }
}

fn load_views(source: &Path) -> Result<Option<ViewsConfig>> {
    let path = source.join("skillfs-views.toml");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A vanished source is not evidence that its views are absent.
            std::fs::read_dir(source).map_err(|error| {
                format!("cannot inspect views in '{}': {error}", source.display())
            })?;
            return Ok(None);
        }
        Err(error) => {
            return Err(format!("cannot inspect views '{}': {error}", path.display()).into());
        }
    }
    // Do not follow `exists()`: it would classify dangling symlinks and some
    // permission failures as absence. Once observed, read failures stay errors.
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read views '{}': {error}", path.display()))?;
    let views = toml::from_str(&text)
        .map_err(|error| format!("invalid views '{}': {error}", path.display()))?;
    Ok(Some(views))
}

fn render_copy(output: &mut impl Write, label: &str, copy: &CopyReport) -> io::Result<()> {
    writeln!(output, "  {label}: {}", escape_for_diagnostics(&copy.path))?;
    writeln!(
        output,
        "    source: {}",
        escape_for_diagnostics(&copy.source)
    )?;
    writeln!(output, "    parse_status: {}", copy.parse_status)?;
    if let Some(message) = &copy.parse_message {
        writeln!(
            output,
            "    parse_message: {}",
            escape_for_diagnostics(message)
        )?;
    }
    match copy.enabled {
        Some(enabled) => writeln!(output, "    enabled: {enabled}"),
        None => writeln!(output, "    enabled: unknown"),
    }
}

fn render_views(
    output: &mut impl Write,
    label: &str,
    views: &Option<Vec<String>>,
) -> io::Result<()> {
    match views {
        None => writeln!(output, "  {label}: unknown"),
        Some(views) if views.is_empty() => writeln!(output, "  {label}: []"),
        Some(views) => {
            writeln!(output, "  {label}:")?;
            for view in views {
                writeln!(output, "    - {}", escape_for_diagnostics(view))?;
            }
            Ok(())
        }
    }
}

fn render_text(output: &mut impl Write, report: &Report) -> io::Result<()> {
    writeln!(
        output,
        "Target: configuration (current files; not an atomic snapshot)"
    )?;
    writeln!(
        output,
        "This diagnosis does not prove a mount exists or an agent has loaded a skill."
    )?;
    writeln!(output, "Status: {}", report.status)?;
    writeln!(
        output,
        "Config: {}",
        escape_for_diagnostics(&report.config_path)
    )?;
    writeln!(
        output,
        "Mountpoint: {}",
        report
            .mountpoint
            .as_deref()
            .map(escape_for_diagnostics)
            .unwrap_or_else(|| "unknown".to_owned())
    )?;
    if let Some(phase) = report.phase {
        writeln!(output, "Phase: {phase}")?;
    }
    if let Some(blocker) = &report.blocker {
        writeln!(output, "Blocker: {}", escape_for_diagnostics(blocker))?;
    }
    writeln!(output, "Sources (precedence order; confirmed):")?;
    for (index, source) in report.sources.iter().enumerate() {
        writeln!(
            output,
            "  {}. {}",
            index + 1,
            escape_for_diagnostics(source)
        )?;
    }
    writeln!(
        output,
        "Views file: {}",
        report
            .views
            .path
            .as_deref()
            .map(escape_for_diagnostics)
            .unwrap_or_else(|| "unknown".to_owned())
    )?;
    writeln!(output, "Views status: {}", report.views.status)?;
    writeln!(
        output,
        "Effective default view: {}",
        report
            .views
            .effective_default
            .as_deref()
            .map(escape_for_diagnostics)
            .unwrap_or_else(|| {
                if matches!(report.views.status, "loaded" | "absent") {
                    "none"
                } else {
                    "unknown"
                }
                .to_owned()
            })
    )?;
    if let Some(skill) = &report.requested_skill {
        writeln!(output, "Requested skill: {}", escape_for_diagnostics(skill))?;
    }
    let selection = match report.selection_status {
        "not_discovered" => "not discovered",
        other => other,
    };
    writeln!(output, "Selection: {selection}")?;
    for skill in &report.skills {
        writeln!(output, "Skill: {}", escape_for_diagnostics(&skill.name))?;
        render_copy(output, "selected", &skill.selected)?;
        if skill.shadowed.is_empty() {
            writeln!(output, "  shadowed: []")?;
        }
        for copy in &skill.shadowed {
            render_copy(output, "shadowed", copy)?;
        }
        match skill.effective_default {
            Membership::Known(value) => writeln!(output, "  effective_default: {value}")?,
            Membership::Unknown(_) => writeln!(output, "  effective_default: unknown")?,
        }
        writeln!(output, "  default_reason: {}", skill.default_reason)?;
        render_views(output, "configured_views", &skill.configured_views)?;
        render_views(output, "secondary_views", &skill.secondary_views)?;
    }
    Ok(())
}

/// Emit one configuration report, returning whether diagnosis was interrupted.
pub(super) fn run(config: &Path, skill: Option<&str>, format: OutputFormat) -> Result<bool> {
    let mut diagnosis = Diagnosis::new(config, skill);
    diagnosis.inspect(config);
    let report = diagnosis.finish();
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    match format {
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut output, &report)?;
            writeln!(output)?;
        }
        OutputFormat::Text => render_text(&mut output, &report)?,
    }
    output.flush()?;
    Ok(report.status == "incomplete")
}
