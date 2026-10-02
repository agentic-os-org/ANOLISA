//! Application orchestration for the implemented `osbase sandbox` operations.

use std::path::Path;

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::osbase_install::{
    self, OsbaseDomain, OsbaseInstallError, OsbaseInstallOutcome, OsbaseInstallRequest,
    RegisterHandler,
};
use anolisa_core::system_helper::HelperRequest;
use anolisa_platform::ipc::SYSTEM_HELPER_SOCKET;
use anolisa_platform::privilege;

use crate::commands::unsupported_dry_run_error;
use crate::helper_client::{HelperClient, HelperClientError, HelperOperationOutcome};
use crate::response::CliError;

/// Typed input for one implemented sandbox operation.
pub(super) enum SandboxRequest {
    /// Install or preview one sandbox scenario.
    Install {
        target: String,
        intent: ExecutionIntent,
        force: bool,
        skip_verify: bool,
    },
    /// Uninstall or preview one sandbox scenario.
    Uninstall {
        scenario: String,
        intent: ExecutionIntent,
    },
    /// Remove one sandbox scenario through the helper or reject its preview.
    Remove {
        target: String,
        purge: bool,
        intent: ExecutionIntent,
    },
    /// Query sandbox status through the helper.
    Status { target: Option<String> },
}

/// Presentation facts emitted at the point required by the existing output order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SandboxOutputEvent {
    /// Announces a helper-routed install or uninstall before its request is sent.
    Scenario(String),
}

/// Typed terminal result consumed by the command renderer.
#[derive(Debug)]
pub(super) enum SandboxOutcome {
    /// The privileged helper completed an operation and returned its phase summary.
    Helper {
        command: String,
        outcome: HelperOperationOutcome,
    },
    /// A root process ran the direct install pipeline.
    DirectInstall {
        command: String,
        outcome: OsbaseInstallOutcome,
    },
    /// A root process completed the direct uninstall pipeline.
    DirectUninstall,
}

trait DirectSandboxExecutor {
    fn install(
        &self,
        request: &OsbaseInstallRequest,
    ) -> Result<OsbaseInstallOutcome, OsbaseInstallError>;

    fn uninstall(&self, scenario: &str, dry_run: bool) -> Result<String, OsbaseInstallError>;
}

struct SystemDirectSandboxExecutor;

impl DirectSandboxExecutor for SystemDirectSandboxExecutor {
    fn install(
        &self,
        request: &OsbaseInstallRequest,
    ) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
        let env = anolisa_env::EnvService::detect();
        osbase_install::execute_install(request, &env)
    }

    fn uninstall(&self, scenario: &str, dry_run: bool) -> Result<String, OsbaseInstallError> {
        osbase_install::execute_uninstall(scenario, dry_run)
    }
}

enum ExecutionMode {
    ViaHelper(HelperClient),
    Direct,
}

/// Run one sandbox operation against production helper and direct boundaries.
pub(super) fn run(
    request: SandboxRequest,
    output: &mut dyn FnMut(SandboxOutputEvent),
) -> Result<SandboxOutcome, CliError> {
    run_with_dependencies(
        request,
        || HelperClient::connect(Path::new(SYSTEM_HELPER_SOCKET)),
        privilege::is_root(),
        &SystemDirectSandboxExecutor,
        output,
    )
}

fn run_with_dependencies<F>(
    request: SandboxRequest,
    connect: F,
    is_root: bool,
    direct: &dyn DirectSandboxExecutor,
    output: &mut dyn FnMut(SandboxOutputEvent),
) -> Result<SandboxOutcome, CliError>
where
    F: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    if matches!(
        &request,
        SandboxRequest::Remove {
            intent: ExecutionIntent::Plan,
            ..
        }
    ) {
        return Err(unsupported_dry_run_error("osbase sandbox remove"));
    }

    match preflight_with(connect, is_root)? {
        ExecutionMode::ViaHelper(mut client) => execute_via_helper(request, &mut client, output),
        ExecutionMode::Direct => execute_direct(request, direct),
    }
}

fn execute_via_helper(
    request: SandboxRequest,
    client: &mut HelperClient,
    output: &mut dyn FnMut(SandboxOutputEvent),
) -> Result<SandboxOutcome, CliError> {
    let (request, command) = match request {
        SandboxRequest::Install {
            target,
            intent,
            force,
            skip_verify,
        } => {
            output(SandboxOutputEvent::Scenario(target.clone()));
            (
                HelperRequest::OsbaseInstall {
                    scenario: target,
                    register_handler: "none".to_string(),
                    register_runtimeclass: false,
                    config_override: None,
                    set_default: false,
                    force,
                    skip_verify,
                    dry_run: matches!(intent, ExecutionIntent::Plan),
                },
                "osbase sandbox install",
            )
        }
        SandboxRequest::Uninstall { scenario, intent } => {
            output(SandboxOutputEvent::Scenario(scenario.clone()));
            (
                HelperRequest::OsbaseUninstall {
                    scenario,
                    dry_run: matches!(intent, ExecutionIntent::Plan),
                },
                "osbase sandbox uninstall",
            )
        }
        SandboxRequest::Remove { target, purge, .. } => (
            HelperRequest::OsbaseRemove {
                scenario: target,
                purge,
            },
            "osbase sandbox remove",
        ),
        SandboxRequest::Status { target } => (
            HelperRequest::OsbaseStatus { scenario: target },
            "osbase sandbox status",
        ),
    };
    let outcome = client
        .execute(&request)
        .map_err(|error| CliError::Runtime {
            command: command.to_string(),
            reason: helper_operation_error(error),
        })?;
    Ok(SandboxOutcome::Helper {
        command: command.to_string(),
        outcome,
    })
}

fn execute_direct(
    request: SandboxRequest,
    direct: &dyn DirectSandboxExecutor,
) -> Result<SandboxOutcome, CliError> {
    match request {
        SandboxRequest::Install {
            target,
            intent,
            force,
            skip_verify,
        } => {
            let command = format!("osbase sandbox install {target}");
            let request = OsbaseInstallRequest {
                domain: OsbaseDomain::Sandbox,
                target: target.clone(),
                register_handler: RegisterHandler::None,
                register_runtimeclass: false,
                config_override: None,
                set_default: false,
                force,
                skip_verify,
                dry_run: matches!(intent, ExecutionIntent::Plan),
            };
            let outcome = direct
                .install(&request)
                .map_err(|error| map_osbase_error(error, "install", &target))?;
            Ok(SandboxOutcome::DirectInstall { command, outcome })
        }
        SandboxRequest::Uninstall { scenario, intent } => {
            direct
                .uninstall(&scenario, matches!(intent, ExecutionIntent::Plan))
                .map_err(|error| map_osbase_error(error, "uninstall", &scenario))?;
            Ok(SandboxOutcome::DirectUninstall)
        }
        SandboxRequest::Remove { target, .. } => Err(CliError::not_implemented(format!(
            "osbase sandbox remove {target}"
        ))),
        SandboxRequest::Status { .. } => Err(CliError::not_implemented("osbase sandbox status")),
    }
}

fn preflight_with<F>(connect: F, is_root: bool) -> Result<ExecutionMode, CliError>
where
    F: FnOnce() -> Result<HelperClient, HelperClientError>,
{
    match connect() {
        Ok(mut client) => {
            let handshake = client
                .handshake(env!("CARGO_PKG_VERSION"))
                .map_err(|error| CliError::Runtime {
                    command: "osbase".to_string(),
                    reason: preflight_handshake_error(error),
                })?;
            if !handshake.compatible {
                let cli_version = env!("CARGO_PKG_VERSION");
                return Err(CliError::Runtime {
                    command: "osbase".to_string(),
                    reason: format!(
                        "anolisa-system-helper version mismatch \
                         (installed: {}, required: {cli_version}); \
                         run 'sudo anolisa system setup' to upgrade",
                        handshake.helper_version
                    ),
                });
            }
            Ok(ExecutionMode::ViaHelper(client))
        }
        Err(_) if is_root => Ok(ExecutionMode::Direct),
        Err(_) => {
            let exe = std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "anolisa".to_string());
            Err(CliError::PermissionDenied {
                command: "osbase".to_string(),
                reason: "osbase requires root privileges and system-helper is not running"
                    .to_string(),
                hint: Some(format!(
                    "Either:\n  1. Install helper: sudo {exe} system setup\n  \
                     2. Run directly: sudo {exe} osbase ..."
                )),
            })
        }
    }
}

fn preflight_handshake_error(error: HelperClientError) -> String {
    match error {
        HelperClientError::Send { source, .. } => {
            format!("failed to send handshake to system-helper: {source}")
        }
        HelperClientError::Receive { source, .. } => {
            format!("failed to receive handshake from system-helper: {source}")
        }
        HelperClientError::Remote { .. } | HelperClientError::UnexpectedResponse { .. } => {
            "system-helper returned unexpected handshake response".to_string()
        }
        HelperClientError::Connect { path, source } => {
            format!("failed to connect to {}: {source}", path.display())
        }
    }
}

fn helper_operation_error(error: HelperClientError) -> String {
    match error {
        HelperClientError::Send { source, .. } => {
            format!("failed to send request to system-helper: {source}")
        }
        HelperClientError::Receive { source, .. } => {
            format!("failed to receive response from system-helper: {source}")
        }
        HelperClientError::Remote { code, message, .. } => format!("[{code}] {message}"),
        HelperClientError::UnexpectedResponse { response, .. } => {
            format!("unexpected response from system-helper: {response:?}")
        }
        HelperClientError::Connect { path, source } => {
            format!("failed to connect to {}: {source}", path.display())
        }
    }
}

fn map_osbase_error(error: OsbaseInstallError, action: &str, target: &str) -> CliError {
    let command = format!("osbase sandbox {action} {target}");
    match &error {
        OsbaseInstallError::InvalidRequest { .. } | OsbaseInstallError::Unsupported(_) => {
            CliError::InvalidArgument {
                command,
                reason: error.to_string(),
            }
        }
        _ => CliError::Runtime {
            command,
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::io;
    use std::path::PathBuf;

    use anolisa_core::osbase_install::{PhaseResult, PhaseStatus};
    use anolisa_core::system_helper::HelperResponse;

    use super::*;
    use crate::helper_client::ScriptedTransport;

    #[derive(Default)]
    struct FakeDirectExecutor {
        install_results: RefCell<VecDeque<Result<OsbaseInstallOutcome, OsbaseInstallError>>>,
        uninstall_results: RefCell<VecDeque<Result<String, OsbaseInstallError>>>,
        install_requests: RefCell<Vec<OsbaseInstallRequest>>,
        uninstall_requests: RefCell<Vec<(String, bool)>>,
    }

    impl DirectSandboxExecutor for FakeDirectExecutor {
        fn install(
            &self,
            request: &OsbaseInstallRequest,
        ) -> Result<OsbaseInstallOutcome, OsbaseInstallError> {
            self.install_requests.borrow_mut().push(request.clone());
            self.install_results
                .borrow_mut()
                .pop_front()
                .expect("scripted install result")
        }

        fn uninstall(&self, scenario: &str, dry_run: bool) -> Result<String, OsbaseInstallError> {
            self.uninstall_requests
                .borrow_mut()
                .push((scenario.to_string(), dry_run));
            self.uninstall_results
                .borrow_mut()
                .pop_front()
                .expect("scripted uninstall result")
        }
    }

    fn install_outcome(exit_code: i32) -> OsbaseInstallOutcome {
        OsbaseInstallOutcome {
            domain: OsbaseDomain::Sandbox,
            target: "runc".to_string(),
            phases: vec![PhaseResult {
                name: "packages".to_string(),
                status: PhaseStatus::Success,
                message: None,
                duration_ms: None,
            }],
            exit_code,
            warnings: Vec::new(),
            hints: Vec::new(),
        }
    }

    fn connect_error() -> HelperClientError {
        HelperClientError::Connect {
            path: PathBuf::from(SYSTEM_HELPER_SOCKET),
            source: io::Error::new(io::ErrorKind::NotFound, "missing helper socket"),
        }
    }

    fn client_with_responses(
        responses: Vec<HelperResponse>,
    ) -> (HelperClient, std::rc::Rc<RefCell<Vec<HelperRequest>>>) {
        let (transport, sent) =
            ScriptedTransport::new(Vec::new(), responses.into_iter().map(Ok).collect());
        (HelperClient::with_transport(transport), sent)
    }

    #[test]
    fn preflight_preserves_root_fallback_and_non_root_error() {
        let root = preflight_with(|| Err(connect_error()), true).expect("root fallback");
        assert!(matches!(root, ExecutionMode::Direct));

        let error = match preflight_with(|| Err(connect_error()), false) {
            Ok(_) => panic!("non-root connection failure must not fall back"),
            Err(error) => error,
        };
        assert!(matches!(error, CliError::PermissionDenied { .. }));
    }

    #[test]
    fn preflight_preserves_handshake_compatibility() {
        let (compatible, _) = client_with_responses(vec![HelperResponse::HandshakeOk {
            helper_version: env!("CARGO_PKG_VERSION").to_string(),
            compatible: true,
        }]);
        let mode = preflight_with(|| Ok(compatible), false).expect("compatible helper");
        assert!(matches!(mode, ExecutionMode::ViaHelper(_)));

        let (incompatible, _) = client_with_responses(vec![HelperResponse::HandshakeOk {
            helper_version: "0.0.1".to_string(),
            compatible: false,
        }]);
        let error = match preflight_with(|| Ok(incompatible), false) {
            Ok(_) => panic!("incompatible helper must fail"),
            Err(error) => error,
        };
        match error {
            CliError::Runtime { reason, .. } => {
                assert!(reason.contains("version mismatch"));
                assert!(reason.contains("0.0.1"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn preflight_preserves_handshake_transport_errors() {
        let cases = [
            (
                vec![Err(io::Error::new(io::ErrorKind::BrokenPipe, "send"))],
                Vec::new(),
                "failed to send handshake to system-helper",
            ),
            (
                vec![Ok(())],
                vec![Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receive"))],
                "failed to receive handshake from system-helper",
            ),
        ];

        for (send_results, receive_results, expected) in cases {
            let (transport, _) = ScriptedTransport::new(send_results, receive_results);
            let client = HelperClient::with_transport(transport);
            let error = match preflight_with(|| Ok(client), false) {
                Ok(_) => panic!("transport failure must fail preflight"),
                Err(error) => error,
            };
            match error {
                CliError::Runtime { reason, .. } => assert!(reason.contains(expected)),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn remove_preview_rejects_before_connection_for_root_and_non_root() {
        for is_root in [false, true] {
            let connected = Cell::new(false);
            let direct = FakeDirectExecutor::default();
            let mut events = Vec::new();
            let error = run_with_dependencies(
                SandboxRequest::Remove {
                    target: "gvisor".to_string(),
                    purge: true,
                    intent: ExecutionIntent::Plan,
                },
                || {
                    connected.set(true);
                    Err(connect_error())
                },
                is_root,
                &direct,
                &mut |event| events.push(event),
            )
            .expect_err("remove preview must fail closed");

            assert!(!connected.get(), "preview must not connect to the helper");
            assert!(events.is_empty());
            assert!(direct.install_requests.borrow().is_empty());
            assert!(direct.uninstall_requests.borrow().is_empty());
            assert_eq!(error.command(), "osbase sandbox remove");
            assert_eq!(error.code(), "INVALID_ARGUMENT");
            assert_eq!(error.exit_code(), 2);
            assert_eq!(
                error.reason(),
                "command 'osbase sandbox remove' does not support --dry-run; no action was taken"
            );
        }
    }

    #[test]
    fn helper_install_preserves_request_and_scenario_event() {
        let (client, sent) = client_with_responses(vec![
            HelperResponse::HandshakeOk {
                helper_version: env!("CARGO_PKG_VERSION").to_string(),
                compatible: true,
            },
            HelperResponse::Success {
                message: "phase summary".to_string(),
                exit_code: 2,
            },
        ]);
        let direct = FakeDirectExecutor::default();
        let mut events = Vec::new();

        let outcome = run_with_dependencies(
            SandboxRequest::Install {
                target: "runc".to_string(),
                intent: ExecutionIntent::Plan,
                force: true,
                skip_verify: true,
            },
            || Ok(client),
            false,
            &direct,
            &mut |event| events.push(event),
        )
        .expect("helper install");

        assert_eq!(
            events,
            vec![SandboxOutputEvent::Scenario("runc".to_string())]
        );
        assert!(matches!(
            outcome,
            SandboxOutcome::Helper {
                outcome: HelperOperationOutcome { exit_code: 2, .. },
                ..
            }
        ));
        assert_eq!(
            sent.borrow().as_slice(),
            [
                HelperRequest::Handshake {
                    cli_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                HelperRequest::OsbaseInstall {
                    scenario: "runc".to_string(),
                    register_handler: "none".to_string(),
                    register_runtimeclass: false,
                    config_override: None,
                    set_default: false,
                    force: true,
                    skip_verify: true,
                    dry_run: true,
                },
            ]
        );
        assert!(direct.install_requests.borrow().is_empty());
    }

    #[test]
    fn helper_maps_uninstall_remove_and_status_requests() {
        let cases = [
            (
                SandboxRequest::Uninstall {
                    scenario: "gvisor".to_string(),
                    intent: ExecutionIntent::Apply,
                },
                HelperRequest::OsbaseUninstall {
                    scenario: "gvisor".to_string(),
                    dry_run: false,
                },
                Some(SandboxOutputEvent::Scenario("gvisor".to_string())),
            ),
            (
                SandboxRequest::Uninstall {
                    scenario: "gvisor".to_string(),
                    intent: ExecutionIntent::Plan,
                },
                HelperRequest::OsbaseUninstall {
                    scenario: "gvisor".to_string(),
                    dry_run: true,
                },
                Some(SandboxOutputEvent::Scenario("gvisor".to_string())),
            ),
            (
                SandboxRequest::Remove {
                    target: "gvisor".to_string(),
                    purge: true,
                    intent: ExecutionIntent::Apply,
                },
                HelperRequest::OsbaseRemove {
                    scenario: "gvisor".to_string(),
                    purge: true,
                },
                None,
            ),
            (
                SandboxRequest::Status {
                    target: Some("gvisor".to_string()),
                },
                HelperRequest::OsbaseStatus {
                    scenario: Some("gvisor".to_string()),
                },
                None,
            ),
        ];

        for (request, expected, expected_event) in cases {
            let (client, sent) = client_with_responses(vec![
                HelperResponse::HandshakeOk {
                    helper_version: env!("CARGO_PKG_VERSION").to_string(),
                    compatible: true,
                },
                HelperResponse::Success {
                    message: "ok".to_string(),
                    exit_code: 0,
                },
            ]);
            let direct = FakeDirectExecutor::default();
            let mut events = Vec::new();
            run_with_dependencies(request, || Ok(client), false, &direct, &mut |event| {
                events.push(event)
            })
            .expect("helper operation");

            assert_eq!(sent.borrow()[1], expected);
            assert_eq!(events, expected_event.into_iter().collect::<Vec<_>>());
        }
    }

    #[test]
    fn helper_operation_preserves_remote_and_unexpected_errors() {
        let cases = [
            (
                HelperResponse::Error {
                    code: "DENIED".to_string(),
                    message: "no access".to_string(),
                },
                "[DENIED] no access",
            ),
            (
                HelperResponse::HandshakeOk {
                    helper_version: "0.3.2".to_string(),
                    compatible: true,
                },
                "unexpected response from system-helper",
            ),
        ];

        for (response, expected) in cases {
            let (client, _) = client_with_responses(vec![
                HelperResponse::HandshakeOk {
                    helper_version: env!("CARGO_PKG_VERSION").to_string(),
                    compatible: true,
                },
                response,
            ]);
            let direct = FakeDirectExecutor::default();
            let error = run_with_dependencies(
                SandboxRequest::Status { target: None },
                || Ok(client),
                false,
                &direct,
                &mut |_| {},
            )
            .expect_err("response must fail");

            match error {
                CliError::Runtime { reason, .. } => assert!(reason.contains(expected)),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn helper_operation_preserves_send_and_receive_errors() {
        let cases = [
            (
                vec![
                    Ok(()),
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "send")),
                ],
                vec![Ok(HelperResponse::HandshakeOk {
                    helper_version: env!("CARGO_PKG_VERSION").to_string(),
                    compatible: true,
                })],
                "failed to send request to system-helper",
            ),
            (
                vec![Ok(()), Ok(())],
                vec![
                    Ok(HelperResponse::HandshakeOk {
                        helper_version: env!("CARGO_PKG_VERSION").to_string(),
                        compatible: true,
                    }),
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receive")),
                ],
                "failed to receive response from system-helper",
            ),
        ];

        for (send_results, receive_results, expected) in cases {
            let (transport, _) = ScriptedTransport::new(send_results, receive_results);
            let client = HelperClient::with_transport(transport);
            let direct = FakeDirectExecutor::default();
            let error = run_with_dependencies(
                SandboxRequest::Status { target: None },
                || Ok(client),
                false,
                &direct,
                &mut |_| {},
            )
            .expect_err("transport failure must fail the operation");

            match error {
                CliError::Runtime { reason, .. } => assert!(reason.contains(expected)),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn root_fallback_injects_direct_install_and_uninstall() {
        let direct = FakeDirectExecutor::default();
        direct
            .install_results
            .borrow_mut()
            .push_back(Ok(install_outcome(0)));
        let install = run_with_dependencies(
            SandboxRequest::Install {
                target: "runc".to_string(),
                intent: ExecutionIntent::Plan,
                force: true,
                skip_verify: true,
            },
            || Err(connect_error()),
            true,
            &direct,
            &mut |_| {},
        )
        .expect("direct install");
        assert!(matches!(install, SandboxOutcome::DirectInstall { .. }));
        let install_requests = direct.install_requests.borrow();
        assert_eq!(install_requests.len(), 1);
        assert_eq!(install_requests[0].target, "runc");
        assert!(install_requests[0].dry_run);
        assert!(install_requests[0].force);
        assert!(install_requests[0].skip_verify);
        drop(install_requests);

        direct
            .uninstall_results
            .borrow_mut()
            .push_back(Ok("removed".to_string()));
        let uninstall = run_with_dependencies(
            SandboxRequest::Uninstall {
                scenario: "gvisor".to_string(),
                intent: ExecutionIntent::Apply,
            },
            || Err(connect_error()),
            true,
            &direct,
            &mut |_| {},
        )
        .expect("direct uninstall");
        assert!(matches!(uninstall, SandboxOutcome::DirectUninstall));
        assert_eq!(
            direct.uninstall_requests.borrow().as_slice(),
            &[("gvisor".to_string(), false)]
        );
        direct
            .uninstall_results
            .borrow_mut()
            .push_back(Ok("planned".to_string()));

        let uninstall_preview = run_with_dependencies(
            SandboxRequest::Uninstall {
                scenario: "gvisor".to_string(),
                intent: ExecutionIntent::Plan,
            },
            || Err(connect_error()),
            true,
            &direct,
            &mut |_| {},
        )
        .expect("direct uninstall preview");

        assert!(matches!(uninstall_preview, SandboxOutcome::DirectUninstall));
        assert_eq!(
            direct.uninstall_requests.borrow().as_slice(),
            &[("gvisor".to_string(), false), ("gvisor".to_string(), true),]
        );
    }

    #[test]
    fn direct_errors_keep_invalid_and_runtime_classification() {
        let direct = FakeDirectExecutor::default();
        direct
            .install_results
            .borrow_mut()
            .push_back(Err(OsbaseInstallError::InvalidRequest {
                reason: "bad scenario".to_string(),
            }));
        let invalid = run_with_dependencies(
            SandboxRequest::Install {
                target: "bad".to_string(),
                intent: ExecutionIntent::Apply,
                force: false,
                skip_verify: false,
            },
            || Err(connect_error()),
            true,
            &direct,
            &mut |_| {},
        )
        .expect_err("invalid request");
        assert!(matches!(invalid, CliError::InvalidArgument { .. }));

        direct
            .uninstall_results
            .borrow_mut()
            .push_back(Err(OsbaseInstallError::PhaseFailed {
                phase: "uninstall".to_string(),
                message: "dnf failed".to_string(),
            }));
        let runtime = run_with_dependencies(
            SandboxRequest::Uninstall {
                scenario: "runc".to_string(),
                intent: ExecutionIntent::Apply,
            },
            || Err(connect_error()),
            true,
            &direct,
            &mut |_| {},
        )
        .expect_err("runtime failure");
        assert!(matches!(runtime, CliError::Runtime { .. }));
    }

    #[test]
    fn root_direct_remove_and_status_remain_unimplemented() {
        let direct = FakeDirectExecutor::default();
        for request in [
            SandboxRequest::Remove {
                target: "runc".to_string(),
                purge: false,
                intent: ExecutionIntent::Apply,
            },
            SandboxRequest::Status { target: None },
        ] {
            let error =
                run_with_dependencies(request, || Err(connect_error()), true, &direct, &mut |_| {})
                    .expect_err("direct operation remains unavailable");
            assert!(matches!(error, CliError::NotImplemented { .. }));
        }

        let error = run_with_dependencies(
            SandboxRequest::Remove {
                target: "runc".to_string(),
                purge: false,
                intent: ExecutionIntent::Apply,
            },
            || Err(connect_error()),
            false,
            &direct,
            &mut |_| {},
        )
        .expect_err("non-root remove requires the helper");
        assert!(matches!(error, CliError::PermissionDenied { .. }));
    }
}
