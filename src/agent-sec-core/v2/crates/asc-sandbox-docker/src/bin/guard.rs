//! Independent Docker sandbox cleanup process launched by the library over a bounded pipe.
#![forbid(unsafe_code)]
fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "asc-sandbox-docker-guard: internal cleanup helper started automatically by DockerBackend"
        );
        return std::process::ExitCode::SUCCESS;
    }
    #[cfg(target_os = "linux")]
    match asc_sandbox_docker::guard_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Docker guard requires Linux");
        std::process::ExitCode::FAILURE
    }
}
