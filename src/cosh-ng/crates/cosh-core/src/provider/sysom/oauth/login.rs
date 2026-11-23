//! Managed browser login task shared by /auth and Guide.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

use super::client::OAuthClient;
use super::contract::{self, OAuthError, CALLBACK_PORT, EXCHANGE_DEADLINE};
use super::loopback::{CallbackListener, READ_TIMEOUT};
use super::store::OAuthStore;

/// Shorter than the five-minute challenge validity so exchange keeps budget.
pub const LOGIN_DEADLINE: Duration = Duration::from_secs(240);

/// System browser handoff; the full URL must never enter argv or logs.
pub trait BrowserOpener: Send + Sync {
    /// Opens the verified authorization URL in the default browser.
    fn open(&self, url: &str) -> Result<(), OAuthError>;
}

/// macOS system URL handoff: the full link travels over stdin, never argv.
pub struct SystemBrowserOpener;

impl BrowserOpener for SystemBrowserOpener {
    fn open(&self, url: &str) -> Result<(), OAuthError> {
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            use std::process::{Command, Stdio};
            let script = format!(
                "open location \"{}\"",
                url.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let mut child = Command::new("osascript")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| OAuthError::BrowserUnavailable)?;
            child
                .stdin
                .take()
                .ok_or(OAuthError::BrowserUnavailable)?
                .write_all(script.as_bytes())
                .map_err(|_| OAuthError::BrowserUnavailable)?;
            use wait_timeout::ChildExt;
            match child.wait_timeout(Duration::from_secs(10)) {
                Ok(Some(status)) if status.success() => Ok(()),
                Ok(_) => {
                    let _ = child.kill();
                    Err(OAuthError::BrowserUnavailable)
                }
                Err(_) => Err(OAuthError::BrowserUnavailable),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = url;
            Err(OAuthError::BrowserUnavailable)
        }
    }
}

/// Safe result: generation and application id only, never credentials.
/// Safe result: generation and application id only, never credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOutcome {
    /// Committed login generation.
    pub generation: u64,
    /// Verified Native application id.
    pub client_id: String,
}

/// Budgets and snapshot for one managed login attempt.
pub struct LoginParams {
    /// Callback port (fixed in production, ephemeral in tests).
    pub port: u16,
    /// Overall browser wait budget.
    pub login_deadline: Duration,
    /// Exchange request budget with network margin.
    pub exchange_deadline: Duration,
    /// `[ai].active_provider` snapshot for supersede detection.
    pub provider_snapshot: Option<String>,
}

impl Default for LoginParams {
    fn default() -> Self {
        Self {
            port: CALLBACK_PORT,
            login_deadline: LOGIN_DEADLINE,
            exchange_deadline: EXCHANGE_DEADLINE,
            provider_snapshot: None,
        }
    }
}

/// Runs challenge → browser → callback → exchange → atomic commit.
/// Cancellation, expiry or a generation race leaves no committed state.
pub async fn run_login(
    client: Arc<OAuthClient>,
    store: Arc<OAuthStore>,
    opener: Arc<dyn BrowserOpener>,
    params: LoginParams,
    mut cancel: watch::Receiver<bool>,
) -> Result<LoginOutcome, OAuthError> {
    if *cancel.borrow() {
        return Err(OAuthError::Cancelled);
    }
    let store_read = store.clone();
    let starting = tokio::task::spawn_blocking(move || store_read.read_state())
        .await
        .map_err(|_| OAuthError::Unavailable)??;
    let listener = CallbackListener::bind(params.port).await?;
    let pkce = contract::Pkce::generate();
    let state = contract::random_state();
    let challenge = tokio::select! {
        biased;
        _ = cancel.changed() => return Err(OAuthError::Cancelled),
        result = client.challenge(&pkce.challenge, &state) => result?,
    };
    let client_id = contract::validate_authorization_url(
        &challenge.authorization_url,
        &state,
        &pkce.challenge,
        &challenge.nonce,
    )?;
    if let Some(expected) = contract::expected_client_id() {
        if client_id != expected {
            return Err(OAuthError::InvalidAuthorizationUrl);
        }
    }
    let opener_task = opener.clone();
    let url = challenge.authorization_url.clone();
    tokio::task::spawn_blocking(move || opener_task.open(&url))
        .await
        .map_err(|_| OAuthError::BrowserUnavailable)??;
    let outcome = tokio::select! {
        biased;
        _ = cancel.changed() => return Err(OAuthError::Cancelled),
        result = listener.wait(&state, params.login_deadline, READ_TIMEOUT) => result?,
    };
    if outcome.error.is_some() {
        return Err(OAuthError::AuthorizationDenied);
    }
    let code = outcome.code.ok_or(OAuthError::CallbackInvalid)?;
    if *cancel.borrow() {
        return Err(OAuthError::Cancelled);
    }
    let issued = tokio::select! {
        biased;
        _ = cancel.changed() => return Err(OAuthError::Cancelled),
        result = tokio::time::timeout(
            params.exchange_deadline,
            client.exchange_code(&challenge.nonce, &code, &pkce.verifier),
        ) => result.map_err(|_| OAuthError::Timeout)??,
    };
    let refresh_token = issued.refresh_token.ok_or(OAuthError::InvalidResponse)?;
    if *cancel.borrow() {
        return Err(OAuthError::Cancelled);
    }
    let (login, _replaced) = tokio::task::spawn_blocking(move || {
        store.commit_login(
            &starting,
            &client_id,
            &refresh_token,
            params.provider_snapshot,
        )
    })
    .await
    .map_err(|_| OAuthError::Unavailable)??;
    Ok(LoginOutcome {
        generation: login.generation,
        client_id: login.client_id,
    })
}

#[cfg(test)]
mod tests {
    use super::super::client::TEST_IDENTITY_ENDPOINT;
    use super::super::store::{OAuthSecretBackend, OAuthStore};
    use super::super::test_support::{http_response, success_credentials, OAuthFixture};
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::super::test_support::OAUTH_PORT_LOCK as PORT_LOCK;

    #[derive(Default)]
    struct MemoryBackend {
        slots: StdMutex<std::collections::HashMap<String, String>>,
    }
    impl OAuthSecretBackend for MemoryBackend {
        fn set(&self, slot: &str, token: &str) -> Result<(), OAuthError> {
            self.slots.lock().unwrap().insert(slot.into(), token.into());
            Ok(())
        }
        fn get(&self, slot: &str) -> Result<Option<String>, OAuthError> {
            Ok(self.slots.lock().unwrap().get(slot).cloned())
        }
        fn delete(&self, slot: &str) -> Result<(), OAuthError> {
            self.slots.lock().unwrap().remove(slot);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingOpener {
        opened: StdMutex<Vec<String>>,
    }
    impl BrowserOpener for RecordingOpener {
        fn open(&self, url: &str) -> Result<(), OAuthError> {
            self.opened.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<OAuthStore>,
        client: Arc<OAuthClient>,
        opener: Arc<RecordingOpener>,
        cancel: watch::Sender<bool>,
        rx: watch::Receiver<bool>,
    }

    async fn harness(fixture: &OAuthFixture) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(OAuthStore::new(
            dir.path(),
            Arc::new(MemoryBackend::default()),
        ));
        let client = TEST_IDENTITY_ENDPOINT.sync_scope(fixture.endpoint.clone(), || {
            Arc::new(OAuthClient::new("https://unused.invalid".into()))
        });
        let opener = Arc::new(RecordingOpener::default());
        let (cancel, rx) = watch::channel(false);
        Harness {
            _dir: dir,
            store,
            client,
            opener,
            cancel,
            rx,
        }
    }

    async fn wait_for_open(harness: &Harness) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(url) = harness.opener.opened.lock().unwrap().first() {
                    return url.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    async fn browser_callback(port: u16, query: &str) {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        socket
            .write_all(
                format!("GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response)).await;
    }

    fn state_of(url: &str) -> String {
        reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned()
    }

    #[tokio::test]
    async fn happy_path_commits_and_returns_only_safe_fields() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        let expected_app = super::contract::expected_client_id().unwrap_or("4038181954510891234");
        *fixture.client_id.lock().await = expected_app.to_string();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        ));
        let url = wait_for_open(&harness).await;
        assert!(url.starts_with("https://signin.aliyun.com/oauth2/v1/auth?"));
        browser_callback(
            CALLBACK_PORT,
            &format!("state={}&code=code-1", state_of(&url)),
        )
        .await;
        let outcome = login.await.unwrap().unwrap();
        assert_eq!(outcome.generation, 1);
        assert_eq!(outcome.client_id, expected_app);
        let state = harness.store.read_state().unwrap();
        assert!(state.selected);
        assert_eq!(state.active.as_ref().unwrap().generation, 1);
        assert_eq!(
            harness.store.load_refresh(1).unwrap().as_deref(),
            Some("refresh-canary")
        );
        let bodies = fixture.bodies().await;
        {
            let requests = fixture.requests.lock().await;
            assert!(requests
                .iter()
                .any(|request| request.contains("/cosh-oauth/challenge")));
            assert!(requests
                .iter()
                .any(|request| request.contains("/cosh-oauth/credentials")));
        }
        let exchange = bodies
            .iter()
            .find(|body| body["grantType"] == "authorization_code")
            .unwrap();
        assert_eq!(exchange["code"], "code-1");
        assert!(exchange["codeVerifier"].as_str().unwrap().len() >= 43);
    }

    #[tokio::test]
    async fn cancel_releases_the_port_and_commits_nothing() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        ));
        wait_for_open(&harness).await;
        harness.cancel.send(true).unwrap();
        assert!(matches!(login.await.unwrap(), Err(OAuthError::Cancelled)));
        assert!(harness.store.read_state().unwrap().active.is_none());
        assert!(
            CallbackListener::bind(CALLBACK_PORT).await.is_ok(),
            "port must be released"
        );
    }

    #[tokio::test]
    async fn port_in_use_fails_before_any_challenge() {
        let _serial = PORT_LOCK.lock().await;
        let _occupied = tokio::net::TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
            .await
            .unwrap();
        let fixture = OAuthFixture::start().await;
        let harness = harness(&fixture).await;
        let error = run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, OAuthError::PortInUse);
        assert!(fixture.requests.lock().await.is_empty());
        assert!(harness.opener.opened.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn matched_denial_terminates_without_commit() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        ));
        let url = wait_for_open(&harness).await;
        browser_callback(
            CALLBACK_PORT,
            &format!("state={}&error=access_denied", state_of(&url)),
        )
        .await;
        assert!(matches!(
            login.await.unwrap(),
            Err(OAuthError::AuthorizationDenied)
        ));
        assert!(harness.store.read_state().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn unvalidated_url_never_reaches_the_browser() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.challenge_response.lock().await = http_response(
            "200 OK",
            &serde_json::json!({"code":"Success","success":true,"data":{
                "nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "authorizationUrl":"https://evil.example/oauth2/v1/auth"}})
            .to_string(),
        );
        let harness = harness(&fixture).await;
        let error = run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, OAuthError::InvalidAuthorizationUrl);
        assert!(harness.opener.opened.lock().unwrap().is_empty());
        assert!(CallbackListener::bind(CALLBACK_PORT).await.is_ok());
    }

    #[tokio::test]
    async fn exchange_timeout_leaves_no_commit() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        fixture
            .credentials_gate
            .acquire_many(100)
            .await
            .unwrap()
            .forget();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams {
                exchange_deadline: Duration::from_millis(100),
                ..Default::default()
            },
            harness.rx.clone(),
        ));
        let url = wait_for_open(&harness).await;
        browser_callback(
            CALLBACK_PORT,
            &format!("state={}&code=code-1", state_of(&url)),
        )
        .await;
        assert!(matches!(login.await.unwrap(), Err(OAuthError::Timeout)));
        assert!(harness.store.read_state().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn concurrent_login_commit_keeps_the_winner() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        ));
        let url = wait_for_open(&harness).await;
        let starting = harness.store.read_state().unwrap();
        harness
            .store
            .commit_login(&starting, "other-client", "other-refresh", None)
            .unwrap();
        browser_callback(
            CALLBACK_PORT,
            &format!("state={}&code=code-1", state_of(&url)),
        )
        .await;
        assert!(matches!(
            login.await.unwrap(),
            Err(OAuthError::GenerationChanged)
        ));
        let state = harness.store.read_state().unwrap();
        assert_eq!(state.active.as_ref().unwrap().client_id, "other-client");
        assert_eq!(
            harness.store.load_refresh(1).unwrap().as_deref(),
            Some("other-refresh")
        );
    }

    #[tokio::test]
    async fn first_issue_without_refresh_token_is_invalid() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.credentials_response.lock().await =
            http_response("200 OK", &success_credentials(900, false));
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        let harness = harness(&fixture).await;
        let login = tokio::spawn(run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        ));
        let url = wait_for_open(&harness).await;
        browser_callback(
            CALLBACK_PORT,
            &format!("state={}&code=code-1", state_of(&url)),
        )
        .await;
        assert!(matches!(
            login.await.unwrap(),
            Err(OAuthError::InvalidResponse)
        ));
        assert!(harness.store.read_state().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn login_deadline_expires_without_callback() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        *fixture.client_id.lock().await = super::contract::expected_client_id()
            .unwrap_or("4038181954510891234")
            .to_string();
        let harness = harness(&fixture).await;
        let error = run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams {
                login_deadline: Duration::from_millis(150),
                ..Default::default()
            },
            harness.rx.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, OAuthError::CallbackTimeout);
    }

    #[cfg(feature = "oauth-pre")]
    #[tokio::test]
    async fn pre_profile_rejects_a_foreign_application() {
        let _serial = PORT_LOCK.lock().await;
        let fixture = OAuthFixture::start().await;
        let harness = harness(&fixture).await;
        let error = run_login(
            harness.client.clone(),
            harness.store.clone(),
            harness.opener.clone(),
            LoginParams::default(),
            harness.rx.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, OAuthError::InvalidAuthorizationUrl);
        assert!(harness.opener.opened.lock().unwrap().is_empty());
        assert!(harness.store.read_state().unwrap().active.is_none());
    }

    /// Real-machine macOS browser handoff canary: the URL must reach the default
    /// browser over stdin (never argv). Opens a local page; run with --ignored.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "provisioned pre-release OAuth acceptance; requires independent login/route/macOS desktop"]
    async fn macos_browser_handoff_canary() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = "cosh oauth browser handoff canary - you can close this tab";
            let _ = socket.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
            ).await;
            request
        });
        let url = format!("http://127.0.0.1:{port}/cosh-oauth-canary");
        let opener = SystemBrowserOpener;
        let opened = tokio::task::spawn_blocking(move || opener.open(&url))
            .await
            .unwrap();
        opened.unwrap();
        let request = tokio::time::timeout(Duration::from_secs(20), server)
            .await
            .expect("browser did not reach the local canary page")
            .unwrap();
        assert!(request.starts_with("GET /cosh-oauth-canary "), "{request}");
    }
}
