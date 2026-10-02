use std::time::Instant;

use clap::Subcommand;

use cosh_platform::detect::Distro;
use cosh_platform::pkg;
use cosh_platform::validate::{validate_pkg_name, validate_pkg_search_query};

use crate::{build_meta, print_failure, print_success};

#[derive(Subcommand)]
pub enum PkgCommands {
    /// Install a package
    Install {
        /// Package name to install
        package: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a package
    Remove {
        /// Package name to remove
        package: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Search for packages
    Search {
        /// Search query
        query: String,
    },
    /// List packages
    List {
        /// Only show installed packages
        #[arg(long)]
        installed: bool,
    },
}

pub fn run(action: PkgCommands, distro: &Distro, start: Instant) -> i32 {
    match action {
        PkgCommands::Install { package, dry_run } => {
            if let Err(e) = validate_pkg_name(&package) {
                return print_failure(e, build_meta("pkg", distro, start, dry_run));
            }
            match pkg::pkg_install(distro, &package, dry_run) {
                Ok(result) => print_success(result, build_meta("pkg", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("pkg", distro, start, dry_run)),
            }
        }
        PkgCommands::Remove { package, dry_run } => {
            if let Err(e) = validate_pkg_name(&package) {
                return print_failure(e, build_meta("pkg", distro, start, dry_run));
            }
            match pkg::pkg_remove(distro, &package, dry_run) {
                Ok(result) => print_success(result, build_meta("pkg", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("pkg", distro, start, dry_run)),
            }
        }
        PkgCommands::Search { query } => {
            if let Err(e) = validate_pkg_search_query(&query) {
                return print_failure(e, build_meta("pkg", distro, start, false));
            }
            match pkg::pkg_search(distro, &query) {
                Ok(result) => print_success(result, build_meta("pkg", distro, start, false)),
                Err(e) => print_failure(e, build_meta("pkg", distro, start, false)),
            }
        }
        PkgCommands::List { installed } => match pkg::pkg_list(distro, installed) {
            Ok(result) => print_success(result, build_meta("pkg", distro, start, false)),
            Err(e) => print_failure(e, build_meta("pkg", distro, start, false)),
        },
    }
}
