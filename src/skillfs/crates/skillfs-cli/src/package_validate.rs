//! Report the experimental package validator's static discovery results.

use std::error::Error;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;
use skillfs_core::plugin::{self, McpServer, PluginDiagnostic, PluginManifest};

use crate::{OutputFormat, escape_ctl};

#[derive(Serialize)]
struct SkillView<'a> {
    name: &'a str,
    path: &'a str,
}

#[derive(Serialize)]
struct PackageReport<'a> {
    experimental: bool,
    accepted: bool,
    manifest: Option<&'a PluginManifest>,
    skills: Vec<SkillView<'a>>,
    servers: &'a [McpServer],
    diagnostics: &'a [PluginDiagnostic],
}

pub(crate) fn run(root: &Path, format: OutputFormat) -> (Result<(), Box<dyn Error>>, bool) {
    let validation = plugin::load_package(root, None);
    let package = validation.package.as_ref();
    let report = PackageReport {
        experimental: true,
        accepted: validation.accepted(),
        manifest: package.map(|package| &package.manifest),
        skills: package
            .iter()
            .flat_map(|package| &package.skills)
            .map(|skill| SkillView {
                name: &skill.name,
                path: &skill.path,
            })
            .collect(),
        servers: package.map_or(&[], |package| package.servers.as_slice()),
        diagnostics: &validation.diagnostics,
    };

    (report.write(root, format), !report.accepted)
}

impl PackageReport<'_> {
    fn write(&self, root: &Path, format: OutputFormat) -> Result<(), Box<dyn Error>> {
        let mut out = io::stdout().lock();
        match format {
            OutputFormat::Json => {
                serde_json::to_writer_pretty(&mut out, self)?;
                writeln!(out)?;
            }
            OutputFormat::Text => self.write_text(&mut out, root)?,
        }
        out.flush()?;
        Ok(())
    }

    fn write_text(&self, out: &mut impl Write, root: &Path) -> io::Result<()> {
        let root = escape_ctl(&root.display().to_string());
        if let Some(manifest) = self.manifest {
            writeln!(
                out,
                "Accepted package {} ({root}) [experimental]",
                escape_ctl(&manifest.name)
            )?;
        } else {
            writeln!(out, "Rejected package {root} [experimental]")?;
        }
        for skill in &self.skills {
            writeln!(
                out,
                "  skill: {} ({})",
                escape_ctl(skill.name),
                escape_ctl(skill.path)
            )?;
        }
        for server in self.servers {
            writeln!(
                out,
                "  server: {} ({})",
                escape_ctl(&server.name),
                server.transport.as_str()
            )?;
        }
        for diagnostic in self.diagnostics {
            writeln!(
                out,
                "  {} [{}] {}{} ({}): {}",
                diagnostic.severity.as_str(),
                diagnostic.component.as_str(),
                escape_ctl(&diagnostic.path),
                escape_ctl(&diagnostic.json_pointer),
                diagnostic.spec_ref,
                diagnostic.message,
            )?;
        }
        Ok(())
    }
}
