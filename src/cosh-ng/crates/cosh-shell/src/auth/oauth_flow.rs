//! Browser login flow shared by /auth and Guide management.
//!
//! The login runs in a dedicated persistent `cosh-core --oauth-login` child
//! (the registry core is short-lived and cannot host the browser wait). The
//! shell only observes safe JSON events; closing the child's stdin cancels.

use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapter::AdapterInstance;
use crate::runtime::state::InlineState;

use super::prompt::{clear_active_auth_panel, render_current_auth_panel};

const POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    WaitingBrowser,
    Cancelling,
}

#[derive(Debug)]
pub(crate) struct OAuthFlow {
    stage: Stage,
    last_poll: Instant,
    child: Option<Child>,
    events: Arc<Mutex<String>>,
}

impl OAuthFlow {
    pub(crate) fn waiting(&self) -> bool {
        matches!(self.stage, Stage::WaitingBrowser | Stage::Cancelling)
    }
}

fn set_status(state: &mut InlineState, message: String) {
    if let Some(auth) = state.auth.state.as_mut() {
        auth.error_message = Some(message);
    }
}

fn redraw<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<()> {
    clear_active_auth_panel(state, output)?;
    render_current_auth_panel(state, output)
}

fn failure_hint(code: &str) -> String {
    match code {
        "Cosh.OAuth.PortInUse" => "回调端口 18763 被占用，请释放后重试。".into(),
        "Cosh.OAuth.CallbackTimeout" => "授权超时，请重新发起登录。".into(),
        "Cosh.OAuth.AuthorizationDenied" => "授权被拒绝，请重新登录或请管理员确认应用范围。".into(),
        "Cosh.OAuth.Cancelled" => "已取消阿里云登录。".into(),
        "Sysom.Identity.PermissionDenied" => "授权失效，请重新登录或联系管理员。".into(),
        "Sysom.Identity.SysomNotActivated" => "账号未开通 SysOM，请开通或修复服务关联角色。".into(),
        "Cosh.OAuth.BrowserUnavailable" => "无法打开系统浏览器，请检查桌面环境。".into(),
        "Cosh.OAuth.SecretStoreUnavailable" => "系统安全存储不可用，已保留原有配置。".into(),
        other => format!("阿里云登录失败（{other}），请重试。"),
    }
}

pub(crate) fn start<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let program = match adapter {
        AdapterInstance::CoshCore(core) => core.program.clone(),
        _ => {
            set_status(state, "当前适配器不支持阿里云账号登录。".into());
            return redraw(state, output);
        }
    };
    let spawned = Command::new(program)
        .arg("--oauth-login")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            let stdout = child.stdout.take().expect("piped stdout");
            let events = Arc::new(Mutex::new(String::new()));
            let sink = events.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut stdout = stdout;
                let mut buf = [0u8; 4096];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => sink
                            .lock()
                            .unwrap()
                            .push_str(&String::from_utf8_lossy(&buf[..n])),
                    }
                }
            });
            state.auth.oauth = Some(OAuthFlow {
                stage: Stage::WaitingBrowser,
                last_poll: Instant::now(),
                child: Some(child),
                events,
            });
            set_status(
                state,
                "浏览器已打开，请完成阿里云账号授权；返回终端后自动继续，Esc 取消。".into(),
            );
        }
        Err(error) => {
            set_status(state, format!("无法启动阿里云登录：{error}"));
        }
    }
    redraw(state, output)
}

pub(crate) fn request_cancel<W: Write>(state: &mut InlineState, output: &mut W) -> io::Result<()> {
    if let Some(flow) = state.auth.oauth.as_mut() {
        flow.stage = Stage::Cancelling;
        if let Some(child) = flow.child.as_mut() {
            let _ = child.stdin.take();
        }
    }
    set_status(state, "正在取消阿里云登录…".into());
    redraw(state, output)
}

enum Outcome {
    Done { generation: u64 },
    Failed { code: String },
}

fn pending_outcome(events: &str) -> Option<Outcome> {
    for line in events.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("phase").and_then(Value::as_str) {
            Some("done") => {
                return Some(Outcome::Done {
                    generation: value.get("generation").and_then(Value::as_u64).unwrap_or(0),
                });
            }
            Some("failed") => {
                return Some(Outcome::Failed {
                    code: value
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("Cosh.OAuth.Unavailable")
                        .to_string(),
                });
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn poll<W: Write>(
    _adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let Some(flow) = state.auth.oauth.as_mut() else {
        return Ok(());
    };
    if flow.last_poll.elapsed() < POLL_INTERVAL {
        return Ok(());
    }
    flow.last_poll = Instant::now();
    let outcome = pending_outcome(&flow.events.lock().unwrap());
    let Some(outcome) = outcome else {
        return Ok(());
    };
    if let Some(mut child) = flow.child.take() {
        let _ = child.wait();
    }
    state.auth.oauth = None;
    match outcome {
        Outcome::Done { generation } => {
            if let Some(auth) = state.auth.state.as_mut() {
                auth.sysom.refresh_oauth("oauth_active");
            }
            set_status(
                state,
                format!("阿里云账号登录成功（代际 {generation}）。切换账号后请开始新会话。"),
            );
        }
        Outcome::Failed { code } => {
            if code == "Sysom.Identity.PermissionDenied" {
                if let Some(auth) = state.auth.state.as_mut() {
                    auth.sysom.refresh_oauth("oauth_needs_login");
                }
                set_status(state, failure_hint(&code));
            } else if code == "Sysom.Identity.SysomNotActivated" {
                set_status(
                    state,
                    format!(
                        "{}\n开通或修复服务关联角色后，请重新发起登录（将使用新的 challenge）。",
                        crate::ui::trial_layout::activation_guidance(state.language)
                    ),
                );
            } else {
                set_status(state, failure_hint(&code));
            }
        }
    }
    redraw(state, output)
}

pub(crate) fn logout<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let result = match adapter {
        AdapterInstance::CoshCore(core) => {
            core.registry_query("auth", "oauth_logout", serde_json::json!({}))
        }
        _ => Err("当前适配器不支持阿里云账号登录".to_string()),
    };
    let message = match result {
        Ok(value) => {
            let removed = value.get("removed_generation").and_then(Value::as_u64);
            let remote = if value.get("remote_revoked").and_then(Value::as_bool) == Some(true) {
                "远端撤销：成功".to_string()
            } else if let Some(code) = value.get("remote_error_code").and_then(Value::as_str) {
                format!("远端撤销：失败（{code}），可在阿里云控制台管理授权")
            } else {
                "远端撤销：未执行".to_string()
            };
            match removed {
                Some(generation) => format!("本机退出：完成（代际 {generation}）；{remote}。"),
                None => "当前没有生效的阿里云登录。".to_string(),
            }
        }
        Err(error) => format!("退出失败：{error}"),
    };
    if let Some(auth) = state.auth.state.as_mut() {
        auth.sysom.refresh_oauth("not_selected");
    }
    set_status(state, message);
    redraw(state, output)
}
