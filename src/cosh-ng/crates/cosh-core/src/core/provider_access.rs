//! Run-local admission and cancellation for ECS identity model requests.

use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, Lines};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep_until, Instant};

use crate::protocol::ecs_trial::TrialAction;
use crate::protocol::{
    EcsTrialContext, EcsTrialControl, EcsTrialProgress, InputMessage, OutputMessage,
    ShellControlRequest,
};
use crate::provider::{
    ContentGenerator, DispatchState, GenerateConfig, GenerateEvent, GenerateStream, Message,
    ProviderFailure, ProviderFailureKind, ToolDeclaration,
};

const ADMISSION_LIMIT: Duration = Duration::from_secs(600);
const MAX_ATTEMPTS: usize = 70;
const MAX_INPUT_MESSAGES: usize = 128;
const MAX_INPUT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Running,
    Paused,
    Stop { paused: bool },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Calling,
    Waiting,
    Paused,
    Accepted,
    Finished,
}

struct State {
    run_id: String,
    flow_id: String,
    revision: u64,
    last_action: Option<TrialAction>,
    stage: Stage,
    active: bool,
    stopped: bool,
    waited: bool,
    dispatch: DispatchState,
    failure: Option<ProviderFailure>,
    input: Vec<String>,
    input_bytes: usize,
    progress: Option<EcsTrialProgress>,
}

/// One validated user run; every model step shares cancellation and failure state.
pub(crate) struct TrialAccess {
    state: Mutex<State>,
    control: watch::Sender<Mode>,
    progress_tx: mpsc::UnboundedSender<EcsTrialProgress>,
    progress_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<EcsTrialProgress>>,
    inner: Arc<dyn ContentGenerator>,
    #[cfg(test)]
    preference_check: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl TrialAccess {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Returns only the terminal, typed failure for the current run.
    pub(crate) fn failure(&self) -> Option<ProviderFailure> {
        self.state().failure.clone()
    }

    /// Reports whether this run has permanently stopped accepting model steps.
    pub(crate) fn stopped(&self) -> bool {
        self.state().stopped
    }

    /// Returns buffered non-trial messages in their original order.
    pub(crate) fn drain_input(&self) -> Vec<String> {
        let mut state = self.state();
        state.input_bytes = 0;
        std::mem::take(&mut state.input)
    }

    fn emit(
        &self,
        state: &mut State,
        phase: &str,
        code: Option<String>,
        terminated: bool,
        resume: bool,
    ) {
        let progress = EcsTrialProgress {
            run_id: state.run_id.clone(),
            flow_id: state.flow_id.clone(),
            revision: state.revision,
            phase: phase.into(),
            code,
            dispatch_state: dispatch_label(state.dispatch),
            local_terminated: terminated,
            resume_allowed: resume,
        };
        state.progress = Some(progress.clone());
        let _ = self.progress_tx.send(progress);
    }

    fn begin(&self) -> Result<(), String> {
        let mut state = self.state();
        if state.stopped || matches!(*self.control.borrow(), Mode::Stop { .. }) {
            return Err(state
                .failure
                .as_ref()
                .map(|failure| failure.code.clone())
                .unwrap_or_else(|| "trial_cancelled".into()));
        }
        state.flow_id = uuid::Uuid::new_v4().to_string();
        state.revision = 1;
        state.last_action = None;
        state.stage = Stage::Calling;
        state.active = false;
        state.waited = false;
        state.dispatch = DispatchState::NotDispatched;
        state.progress = None;
        Ok(())
    }

    fn calling(&self) {
        let mut state = self.state();
        state.stage = Stage::Calling;
        state.active = true;
        state.dispatch = DispatchState::Unknown;
    }

    fn waiting(&self, failure: &ProviderFailure) {
        let mut state = self.state();
        state.active = false;
        state.stage = Stage::Waiting;
        state.dispatch = DispatchState::NotDispatched;
        let phase = if failure.kind == ProviderFailureKind::CreditsPreparing {
            "credits_preparing"
        } else {
            "waiting_activation"
        };
        if !state.waited
            || state
                .progress
                .as_ref()
                .is_none_or(|progress| progress.phase != phase)
        {
            state.waited = true;
            self.emit(&mut state, phase, Some(failure.code.clone()), false, false);
        }
    }

    fn accepted(&self, first: &GenerateEvent) {
        let mut state = self.state();
        state.stage = Stage::Accepted;
        state.active = true;
        state.dispatch = DispatchState::Accepted;
        if state.waited && real_content(first) {
            state.waited = false;
            self.emit(&mut state, "ready", None, false, false);
        }
    }

    fn finish(&self) {
        let mut state = self.state();
        state.active = false;
        state.stage = Stage::Finished;
    }

    fn fail(&self, failure: ProviderFailure) {
        let mut state = self.state();
        state.active = false;
        state.stopped = true;
        state.dispatch = failure.dispatch;
        state.failure = Some(failure.clone());
        self.control.send_replace(Mode::Stop { paused: false });
        self.emit(&mut state, "failed", Some(failure.code), true, false);
    }

    fn request_stop(&self, paused: bool) {
        let acknowledge = {
            let mut state = self.state();
            state.stopped = true;
            if paused && state.active {
                state.dispatch = DispatchState::Unknown;
            }
            if state.failure.is_none() {
                state.failure = Some(local_failure("trial_cancelled", state.dispatch));
            }
            self.control.send_replace(Mode::Stop { paused });
            !state.active
                && matches!(
                    state.stage,
                    Stage::Waiting | Stage::Paused | Stage::Finished
                )
        };
        self.inner.cancel();
        if acknowledge {
            self.acknowledge_stop(paused);
        }
    }

    fn acknowledge_stop(&self, paused: bool) {
        let mut state = self.state();
        let phase = if paused { "paused" } else { "terminated" };
        if state.progress.as_ref().is_some_and(|progress| {
            progress.revision == state.revision
                && progress.phase == phase
                && progress.local_terminated
                && !progress.resume_allowed
        }) {
            return;
        }
        state.active = false;
        state.stage = Stage::Finished;
        state.stopped = true;
        let failure = local_failure("trial_cancelled", state.dispatch);
        if state.failure.is_none() {
            state.failure = Some(failure);
        }
        self.emit(
            &mut state,
            if paused { "paused" } else { "terminated" },
            Some("trial_cancelled".into()),
            true,
            false,
        );
    }

    fn control(&self, control: EcsTrialControl) {
        let mut state = self.state();
        if control.run_id != state.run_id || control.flow_id != state.flow_id {
            return;
        }
        if control.revision == state.revision && state.last_action == Some(control.action) {
            if let Some(progress) = state.progress.clone() {
                let _ = self.progress_tx.send(progress);
            }
            return;
        }
        if state.revision.checked_add(1) != Some(control.revision) {
            return;
        }
        if control.action == TrialAction::Resume {
            if state.stage != Stage::Paused || state.stopped {
                return;
            }
            state.revision = control.revision;
            state.last_action = Some(control.action);
            state.stage = Stage::Waiting;
            self.control.send_replace(Mode::Running);
            self.emit(&mut state, "waiting_activation", None, false, false);
            return;
        }
        state.revision = control.revision;
        state.last_action = Some(control.action);
        if control.action == TrialAction::Pause && state.stage == Stage::Waiting && !state.stopped {
            state.stage = Stage::Paused;
            self.control.send_replace(Mode::Paused);
            self.emit(&mut state, "paused", None, true, true);
            return;
        }
        let paused = control.action == TrialAction::Pause;
        let acknowledge = !state.active
            && matches!(
                state.stage,
                Stage::Waiting | Stage::Paused | Stage::Finished
            );
        drop(state);
        self.request_stop(paused);
        if acknowledge {
            self.acknowledge_stop(paused);
        }
    }

    fn buffer(&self, line: String) {
        let mut state = self.state();
        if state.input.len() >= MAX_INPUT_MESSAGES
            || state.input_bytes.saturating_add(line.len()) > MAX_INPUT_BYTES
        {
            drop(state);
            self.request_stop(false);
            return;
        }
        state.input_bytes += line.len();
        state.input.push(line);
    }

    async fn permitted(&self) -> bool {
        #[cfg(test)]
        if let Some(check) = &self.preference_check {
            return check();
        }
        matches!(
            tokio::task::spawn_blocking(crate::auth::trial_preferences::load).await,
            Ok(Ok(true))
        )
    }
}

fn dispatch_label(dispatch: DispatchState) -> &'static str {
    match dispatch {
        DispatchState::NotDispatched => "not_dispatched",
        DispatchState::Accepted => "accepted",
        DispatchState::Unknown => "unknown",
    }
}
fn local_failure(code: &str, dispatch: DispatchState) -> ProviderFailure {
    ProviderFailure {
        code: code.into(),
        kind: ProviderFailureKind::Other,
        dispatch,
        request_id: None,
    }
}
fn pollable(failure: &ProviderFailure) -> bool {
    failure.dispatch == DispatchState::NotDispatched
        && matches!(
            (failure.kind, failure.code.as_str()),
            (
                ProviderFailureKind::ActivationRequired,
                "Sysom.ServiceNotActivated"
            ) | (
                ProviderFailureKind::CreditsPreparing,
                "Sysom.CreditsPreparing"
            )
        )
}
fn real_content(event: &GenerateEvent) -> bool {
    match event {
        GenerateEvent::TextDelta(text) | GenerateEvent::ThinkingDelta(text) => !text.is_empty(),
        GenerateEvent::ToolCallStart { .. }
        | GenerateEvent::ToolCallDelta { .. }
        | GenerateEvent::ToolCallEnd { .. } => true,
        _ => false,
    }
}

struct TrialProvider {
    inner: Arc<dyn ContentGenerator>,
    access: Arc<TrialAccess>,
}

enum AdmissionError {
    Stop(bool),
    Timeout,
}

async fn running(
    control: &mut watch::Receiver<Mode>,
    deadline: Instant,
) -> Result<(), AdmissionError> {
    loop {
        let mode = *control.borrow_and_update();
        match mode {
            Mode::Running => return Ok(()),
            Mode::Stop { paused } => return Err(AdmissionError::Stop(paused)),
            Mode::Paused => tokio::select! {
                biased;
                changed=control.changed()=>{if changed.is_err(){return Err(AdmissionError::Stop(false));}},
                _=sleep_until(deadline)=>return Err(AdmissionError::Timeout),
            },
        }
    }
}

fn admission_error(access: &TrialAccess, error: AdmissionError) -> String {
    match error {
        AdmissionError::Stop(paused) => {
            access.acknowledge_stop(paused);
            "trial_cancelled".into()
        }
        AdmissionError::Timeout => {
            access.fail(local_failure("trial_wait_timeout", DispatchState::Unknown));
            "trial_wait_timeout".into()
        }
    }
}

#[async_trait]
impl ContentGenerator for TrialProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.access.begin()?;
        let started = Instant::now();
        let deadline = started + ADMISSION_LIMIT;
        let mut control = self.access.control.subscribe();
        let mut attempts = 0;
        loop {
            if let Err(error) = running(&mut control, deadline).await {
                return Err(admission_error(&self.access, error));
            }
            if Instant::now() >= deadline || attempts >= MAX_ATTEMPTS {
                return Err(admission_error(&self.access, AdmissionError::Timeout));
            }
            self.access.calling();
            attempts += 1;
            let result = {
                let request = async {
                    if !self.access.permitted().await {
                        return Err(local_failure(
                            "trial_preferences_disabled",
                            DispatchState::NotDispatched,
                        ));
                    }
                    let mut stream =
                        self.inner
                            .generate(messages, tools, config)
                            .await
                            .map_err(|_| {
                                local_failure("trial_provider_error", DispatchState::Unknown)
                            })?;
                    let first = stream.next().await;
                    Ok((stream, first))
                };
                tokio::pin!(request);
                loop {
                    tokio::select! {
                        biased;
                        changed=control.changed()=>{
                            let mode=*control.borrow_and_update();
                            if changed.is_err(){break Err(AdmissionError::Stop(false));}
                            if let Mode::Stop {paused}=mode {break Err(AdmissionError::Stop(paused));}
                        },
                        _=sleep_until(deadline)=>break Err(AdmissionError::Timeout),
                        result=&mut request=>break Ok(result),
                    }
                }
            };
            let (stream, first) = match result {
                Err(error) => return Err(admission_error(&self.access, error)),
                Ok(Err(failure)) => {
                    self.access.fail(failure.clone());
                    return Ok(failure_stream(failure));
                }
                Ok(Ok(result)) => result,
            };
            match first {
                Some(GenerateEvent::ProviderFailure(failure)) => {
                    drop(stream);
                    if !pollable(&failure) {
                        self.access.fail(failure.clone());
                        return Ok(failure_stream(failure));
                    }
                    self.access.waiting(&failure);
                    let interval = if started.elapsed() < Duration::from_secs(60) {
                        Duration::from_secs(5)
                    } else {
                        Duration::from_secs(10)
                    };
                    let wake = (Instant::now() + interval).min(deadline);
                    let sleep = sleep_until(wake);
                    tokio::pin!(sleep);
                    loop {
                        if let Err(error) = running(&mut control, deadline).await {
                            return Err(admission_error(&self.access, error));
                        }
                        let finished = tokio::select! {
                            biased;
                            changed=control.changed()=>{if changed.is_err(){return Err(admission_error(&self.access,AdmissionError::Stop(false)));}false},
                            _=&mut sleep=>true,
                        };
                        if finished {
                            break;
                        }
                    }
                }
                Some(GenerateEvent::Cancelled) => {
                    drop(stream);
                    self.access.request_stop(false);
                    self.access.acknowledge_stop(false);
                    return Err("trial_cancelled".into());
                }
                None | Some(GenerateEvent::Error(_)) | Some(GenerateEvent::MessageEnd) => {
                    drop(stream);
                    self.access.fail(local_failure(
                        "trial_invalid_response",
                        DispatchState::Unknown,
                    ));
                    return Err("trial_invalid_response".into());
                }
                Some(first) => {
                    self.access.accepted(&first);
                    return Ok(wrap_stream(stream, first, self.access.clone(), control));
                }
            }
        }
    }
    fn cancel(&self) {
        self.access.request_stop(false);
    }
}

fn failure_stream(failure: ProviderFailure) -> GenerateStream {
    Box::pin(futures::stream::iter([GenerateEvent::ProviderFailure(
        failure,
    )]))
}

fn wrap_stream(
    stream: GenerateStream,
    first: GenerateEvent,
    access: Arc<TrialAccess>,
    control: watch::Receiver<Mode>,
) -> GenerateStream {
    let lifecycle = access.clone();
    let stream: GenerateStream = Box::pin(futures::stream::unfold(
        (Some(stream), Some(first), access, control),
        |(mut stream, mut first, access, mut control)| async move {
            stream.as_ref()?;
            let mode = *control.borrow_and_update();
            if let Mode::Stop { paused } = mode {
                drop(stream.take());
                access.acknowledge_stop(paused);
                return Some((GenerateEvent::Cancelled, (stream, None, access, control)));
            }
            let inner = stream.as_mut()?;
            let event = if let Some(event) = first.take() {
                Some(event)
            } else {
                loop {
                    let mode = *control.borrow_and_update();
                    if let Mode::Stop { paused } = mode {
                        drop(stream.take());
                        access.acknowledge_stop(paused);
                        return Some((GenerateEvent::Cancelled, (stream, None, access, control)));
                    }
                    tokio::select! {
                        biased;
                        changed=control.changed()=>{if changed.is_err(){access.request_stop(false);}},
                        event=inner.next()=>break event,
                    }
                }
            };
            match event {
                None => {
                    drop(stream.take());
                    access.finish();
                    None
                }
                Some(GenerateEvent::ProviderFailure(mut failure)) => {
                    drop(stream.take());
                    failure.dispatch = DispatchState::Accepted;
                    access.fail(failure.clone());
                    Some((
                        GenerateEvent::ProviderFailure(failure),
                        (stream, None, access, control),
                    ))
                }
                Some(GenerateEvent::Error(_)) => {
                    drop(stream.take());
                    access.fail(local_failure(
                        "trial_provider_error",
                        DispatchState::Unknown,
                    ));
                    Some((
                        GenerateEvent::Error("trial_provider_error".into()),
                        (stream, None, access, control),
                    ))
                }
                Some(GenerateEvent::Cancelled) => {
                    drop(stream.take());
                    access.request_stop(false);
                    access.acknowledge_stop(false);
                    Some((GenerateEvent::Cancelled, (stream, None, access, control)))
                }
                Some(event) => {
                    if real_content(&event) {
                        access.accepted(&event);
                    }
                    if matches!(event, GenerateEvent::MessageEnd) {
                        drop(stream.take());
                        access.finish();
                    }
                    Some((event, (stream, None, access, control)))
                }
            }
        },
    ));
    Box::pin(AccessStream {
        stream: Some(stream),
        access: lifecycle,
    })
}

struct AccessStream {
    stream: Option<GenerateStream>,
    access: Arc<TrialAccess>,
}

impl futures::Stream for AccessStream {
    type Item = GenerateEvent;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let state = self.get_mut();
        loop {
            let Some(stream) = state.stream.as_mut() else {
                return std::task::Poll::Ready(None);
            };
            match stream.as_mut().poll_next(context) {
                std::task::Poll::Ready(Some(GenerateEvent::RequestAccepted)) => continue,
                event => return event,
            }
        }
    }
}

impl Drop for AccessStream {
    fn drop(&mut self) {
        drop(self.stream.take());
        if !self.access.state().active {
            return;
        }
        let mode = *self.access.control.borrow();
        if let Mode::Stop { paused } = mode {
            self.access.acknowledge_stop(paused);
        } else {
            self.access.finish();
        }
    }
}

/// Wraps one already validated ECS trial run without changing persisted configuration.
pub(crate) fn wrap(
    inner: Arc<dyn ContentGenerator>,
    _context: EcsTrialContext,
) -> (Box<dyn ContentGenerator>, Arc<TrialAccess>) {
    construct(inner)
}

fn construct(inner: Arc<dyn ContentGenerator>) -> (Box<dyn ContentGenerator>, Arc<TrialAccess>) {
    let (control, _) = watch::channel(Mode::Running);
    let (progress_tx, progress_rx) = mpsc::unbounded_channel();
    let access = Arc::new(TrialAccess {
        state: Mutex::new(State {
            run_id: uuid::Uuid::new_v4().to_string(),
            flow_id: String::new(),
            revision: 1,
            last_action: None,
            stage: Stage::Idle,
            active: false,
            stopped: false,
            waited: false,
            dispatch: DispatchState::NotDispatched,
            failure: None,
            input: Vec::new(),
            input_bytes: 0,
            progress: None,
        }),
        control,
        progress_tx,
        progress_rx: tokio::sync::Mutex::new(progress_rx),
        inner: inner.clone(),
        #[cfg(test)]
        preference_check: None,
    });
    (
        Box::new(TrialProvider {
            inner,
            access: access.clone(),
        }),
        access,
    )
}

fn write_progress(writer: &mut impl Write, progress: EcsTrialProgress) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, &OutputMessage::EcsTrialState { progress })?;
    writeln!(writer)?;
    writer.flush()
}

/// Pumps the sole input reader while polling the exact same provider future.
pub(crate) async fn drive<F: Future, R: AsyncBufReadExt + Unpin, W: Write>(
    access: Option<&Arc<TrialAccess>>,
    future: F,
    reader: &mut Lines<R>,
    writer: &mut W,
) -> F::Output {
    let Some(access) = access else {
        return future.await;
    };
    let mut progress = access.progress_rx.lock().await;
    tokio::pin!(future);
    let mut input_open = true;
    loop {
        tokio::select! {
            biased;
            Some(state)=progress.recv()=>{if write_progress(writer,state).is_err(){access.request_stop(false);}},
            line=reader.next_line(),if input_open=>{
                match line {
                    Ok(Some(line))=>match serde_json::from_str::<InputMessage>(&line) {
                        Ok(InputMessage::EcsTrialControl {control})=>access.control(control),
                        Ok(InputMessage::ControlRequest {request:ShellControlRequest::Interrupt,..})=>access.request_stop(false),
                        Ok(InputMessage::ControlRequest {request:ShellControlRequest::Shutdown,..})=>{access.buffer(line);access.request_stop(false);},
                        _=>access.buffer(line),
                    },
                    _=>{input_open=false;access.request_stop(false);},
                }
            },
            output=&mut future=>{
                while let Ok(state)=progress.try_recv(){if write_progress(writer,state).is_err(){access.request_stop(false);}}
                return output;
            },
        }
    }
}

#[cfg(test)]
pub(super) fn wrap_with_check(
    inner: Arc<dyn ContentGenerator>,
    _context: EcsTrialContext,
    check: Arc<dyn Fn() -> bool + Send + Sync>,
) -> (Box<dyn ContentGenerator>, Arc<TrialAccess>) {
    let (provider, access) = construct(inner.clone());
    drop(provider);
    let mut access = Arc::try_unwrap(access).ok().unwrap();
    access.preference_check = Some(check);
    let access = Arc::new(access);
    (
        Box::new(TrialProvider {
            inner,
            access: access.clone(),
        }),
        access,
    )
}

#[cfg(test)]
mod tests;
