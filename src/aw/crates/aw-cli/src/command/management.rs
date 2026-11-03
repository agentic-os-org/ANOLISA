//! Plan package changes without writes, then apply them through the persistent backend.

use super::{package, scope};
use crate::{output, package::signals};
use aw_package::{plan, transaction};
use clap::Args;
use std::path::Path;

#[derive(Args, Debug, PartialEq)]
pub(super) struct Recover {
    /// Package database scope (default: user); system writes require root
    #[arg(long, value_enum)]
    scope: Option<scope::Scope>,
    #[command(flatten)]
    output: output::Options,
}

pub(super) fn install(value: package::Install) -> crate::cli::Result<crate::cli::Exit> {
    let result = (|| {
        let paths = scope::paths(value.scope)?;
        let prefix = value
            .prefix
            .as_deref()
            .ok_or_else(|| aw_package::Error::Invalid("missing --prefix".into()))?;
        let bundle = value
            .bundle
            .as_deref()
            .ok_or_else(|| aw_package::Error::Invalid("missing --bundle".into()))?;
        let plan = plan::install(&paths, Path::new(bundle), Path::new(prefix))?;
        Ok((paths, plan))
    })();
    complete("install", value.dry_run, value.output, result)
}

pub(super) fn uninstall(value: package::Uninstall) -> crate::cli::Result<crate::cli::Exit> {
    let result = (|| {
        let paths = scope::paths(value.scope)?;
        let plan = plan::uninstall(&paths, Path::new(&value.prefix.prefix), &value.packages)?;
        Ok((paths, plan))
    })();
    complete("uninstall", value.dry_run, value.prefix.output, result)
}

fn complete(
    command: &'static str,
    dry_run: bool,
    options: output::Options,
    result: aw_package::Result<(aw_package::database::DatabasePaths, plan::PackagePlan)>,
) -> crate::cli::Result<crate::cli::Exit> {
    let (paths, plan) = match result {
        Ok(value) => value,
        Err(error) => return output::complete(command, options, Err(error)),
    };
    if dry_run || !plan.ready() {
        return output::complete(
            command,
            options,
            Ok(output::Outcome::PackagePlanned {
                plan: Box::new(plan),
            }),
        );
    }
    signals::register()?;
    let outcome = match plan.action {
        plan::Action::Install => output::Outcome::Installed {
            prefix: plan.prefix.clone(),
        },
        plan::Action::Uninstall => output::Outcome::Uninstalled,
    };
    let result = plan::execute(&paths, &plan, &signals::CANCEL);
    output::complete_mutation(command, options, outcome, result)
}

pub(super) fn recover(value: Recover) -> crate::cli::Result<crate::cli::Exit> {
    signals::register()?;
    let result = scope::paths(value.scope).and_then(|paths| {
        let report = transaction::recover(&paths, &signals::CANCEL)?;
        Ok(output::Outcome::PackageRecovered {
            database_path: paths.installed(),
            report,
        })
    });
    output::complete("recover", value.output, result)
}
