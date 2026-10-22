//! Adapt admitted typed options to existing handlers without reparsing argv.

use super::{Agent, Command, Config, Endpoint, Native, Service, Validate};
use crate::cli::{Arguments, Exit, Result};
use std::collections::BTreeMap;

fn options<const N: usize>(entries: [(&str, Option<String>); N]) -> Arguments {
    Arguments {
        flags: entries
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (format!("--{name}"), value)))
            .collect(),
        native: Vec::new(),
    }
}

fn native(value: Native) -> Arguments {
    options([
        ("config", Some(value.config)),
        ("agent", Some(value.agent)),
        ("native-profile", value.native_profile),
    ])
}

fn endpoint(value: Endpoint) -> Arguments {
    options([("config", value.config), ("socket", value.socket)])
}

fn validate(value: Validate) -> Result<Exit> {
    crate::runtime(
        "validate",
        &options([("config", Some(value.config)), ("against", value.against)]),
    )
}

fn service(value: Service) -> Result<Exit> {
    match value {
        Service::Serve(value) => crate::runtime(
            "serve",
            &options([
                ("config", Some(value.config)),
                ("state-dir", Some(value.state_dir)),
            ]),
        ),
        Service::Status(value) => crate::runtime("status", &endpoint(value)),
        Service::Stop(value) => crate::runtime("stop", &endpoint(value)),
    }
}

pub(super) fn run(command: Command) -> Result<Exit> {
    match command {
        Command::Run(value) => {
            let mut args = native(value.native);
            args.flags.extend(
                options([
                    ("native-settings", value.native_settings),
                    ("native-state-dir", value.native_state_dir),
                    ("reloadable", value.reloadable.then(|| "true".into())),
                ])
                .flags,
            );
            args.native = value.arguments;
            crate::runtime("run", &args)
        }
        Command::Install(value) => {
            if value.config.is_some() {
                crate::runtime(
                    "install",
                    &options([
                        ("config", value.config),
                        ("agent", value.agent),
                        ("native-profile", value.native_profile),
                    ]),
                )
            } else {
                package(
                    "install",
                    options([("prefix", value.prefix), ("bundle", value.bundle)]),
                )
            }
        }
        Command::Uninstall(value) => {
            package("uninstall", options([("prefix", Some(value.prefix))]))
        }
        Command::Agent(Agent::Connect(value)) => crate::runtime("install", &native(value)),
        Command::Config(value) => config(value),
        Command::Service(value) => service(value),
        Command::Serve(value) => service(Service::Serve(value)),
        Command::Status(value) => service(Service::Status(value)),
        Command::Stop(value) => service(Service::Stop(value)),
        Command::Validate(value) => validate(value),
        Command::Update(value) => {
            let mut args = endpoint(value.endpoint);
            args.flags.extend(
                options([
                    ("agent", value.agent),
                    ("instance", value.instance),
                    ("policy", Some(value.policy)),
                    ("expected-revision", value.expected_revision),
                ])
                .flags,
            );
            crate::runtime("update", &args)
        }
        Command::Inspect(value) => crate::runtime(
            "inspect",
            &options([
                ("config", Some(value.config)),
                ("json", value.json.then(|| "true".into())),
            ]),
        ),
        Command::Request(value) => crate::runtime(
            "request",
            &options([
                ("socket", Some(value.socket)),
                (
                    "timeout-ms",
                    value.timeout_ms.map(|value| value.to_string()),
                ),
            ]),
        ),
        Command::Policy => crate::runtime("policy", &Arguments::default()),
        Command::Hook(value) => crate::runtime(
            "hook",
            &options([
                ("adapter", Some(value.bridge.adapter)),
                ("binding", Some(value.bridge.binding)),
                ("event", Some(value.event)),
                ("step", Some(value.step)),
                ("on-error", Some(value.on_error)),
                ("close-event", value.close_event),
            ]),
        ),
        Command::Bridge(value) => crate::runtime(
            "bridge",
            &options([
                ("adapter", Some(value.adapter)),
                ("binding", Some(value.binding)),
            ]),
        ),
    }
}

fn package(action: &str, args: Arguments) -> Result<Exit> {
    crate::package::execute(action, &args.flags, args.native, "aw")?;
    Ok(Exit::Code(0))
}

fn config(command: Config) -> Result<Exit> {
    let (action, flags): (_, BTreeMap<_, _>) = match command {
        Config::Validate(value) => return validate(value),
        Config::Init(value) => {
            let mut args = options([
                ("prefix", Some(value.prefix.prefix)),
                ("config", Some(value.config)),
                ("state-dir", Some(value.state_dir)),
                ("qoder", value.qoder),
                ("node", value.node),
                ("openclaw", value.openclaw),
                ("provider", value.provider),
                ("socket", value.socket),
                ("check", value.check),
                ("effect", value.effect),
                ("reason-code", value.reason_code),
            ]);
            args.native = value.arguments;
            return package("configure", args);
        }
        Config::Show(value) => (
            "show",
            options([
                ("config", Some(value.configuration.config)),
                ("provider", value.provider),
                ("event", value.event),
                ("id", value.id),
            ])
            .flags,
        ),
        Config::AddProvider(value) => (
            "add-provider",
            options([
                ("config", Some(value.provider.configuration.config)),
                ("name", Some(value.provider.name)),
                ("definition", Some(value.definition)),
            ])
            .flags,
        ),
        Config::RemoveProvider(value) => (
            "remove-provider",
            options([
                ("config", Some(value.configuration.config)),
                ("name", Some(value.name)),
            ])
            .flags,
        ),
        Config::AddEvent(value) => (
            "add-event",
            options([
                ("config", Some(value.configuration.config)),
                ("event", Some(value.event)),
                ("definition", Some(value.definition)),
            ])
            .flags,
        ),
        Config::AddHook(value) => (
            "add-hook",
            options([
                ("config", Some(value.configuration.config)),
                ("event", Some(value.event)),
                ("definition", Some(value.definition)),
            ])
            .flags,
        ),
        Config::RemoveHook(value) => (
            "remove-hook",
            options([
                ("config", Some(value.configuration.config)),
                ("event", Some(value.event)),
                ("id", Some(value.id)),
            ])
            .flags,
        ),
    };
    crate::package::config::execute(action, &flags)?;
    Ok(Exit::Code(0))
}
