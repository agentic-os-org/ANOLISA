//! Configuration and daemon trust boundaries without a privileged service.

#[cfg(any(feature = "sec-core", feature = "tokenless"))]
use aw_sdk::Error;
use aw_sdk::{Client, Config};

#[test]
fn disabled_components_are_explicit() {
    let client = Client::new(Config::default()).unwrap();
    #[cfg(feature = "sec-core")]
    assert!(matches!(
        client.sec_core(),
        Err(Error::NotConfigured("sec-core"))
    ));
    #[cfg(feature = "tokenless")]
    assert!(matches!(
        client.tokenless(),
        Err(Error::NotConfigured("Tokenless"))
    ));
    #[cfg(not(any(feature = "sec-core", feature = "tokenless")))]
    let _ = client;
}

#[cfg(feature = "tokenless")]
#[test]
fn tokenless_rejects_ambient_state_and_relative_rtk() {
    for config in [
        aw_sdk::tokenless::Config {
            runtime: Default::default(),
            rtk_path: None,
        },
        aw_sdk::tokenless::Config::new("relative"),
        aw_sdk::tokenless::Config {
            rtk_path: Some("rtk".into()),
            ..aw_sdk::tokenless::Config::new("/unused")
        },
    ] {
        assert!(matches!(
            Client::new(Config {
                tokenless: Some(config),
                #[cfg(feature = "sec-core")]
                sec_core_socket: None
            }),
            Err(Error::Configuration(_))
        ));
    }
}

#[cfg(feature = "sec-core")]
#[test]
fn construction_does_not_connect_or_fall_back_to_a_local_scanner() {
    use aw_sdk::{CallContext, sec_core};
    use std::time::Duration;

    let client = Client::new(Config {
        sec_core_socket: Some("/does-not-exist/aw-sdk/daemon.sock".into()),
        #[cfg(feature = "tokenless")]
        tokenless: None,
    })
    .unwrap();
    let result = client.sec_core().unwrap().scan_code(
        &sec_core::ScanRequest {
            code: "echo hello".into(),
            language: "bash".into(),
            rules: None,
            mode: None,
        },
        &CallContext::new("test"),
        Duration::from_secs(1),
    );
    assert!(matches!(
        result,
        Err(Error::SecCore(sec_core::ScanError::Transport(
            sec_core::ClientError::Connect(_)
        )))
    ));
    assert!(matches!(
        Client::new(Config {
            sec_core_socket: Some("daemon.sock".into()),
            #[cfg(feature = "tokenless")]
            tokenless: None
        }),
        Err(Error::Configuration(_))
    ));
}
