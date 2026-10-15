//! Isolated process runtime for the typed UI worker protocol.

use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::{buffer::Buffer, layout::Rect};

use super::approval_surface::render_approval_view;
use super::render_surface::{RenderSurface, RenderSurfaceError};
use super::worker_protocol::{
    decode_host_message, encode_worker_message, FrameKey, Heartbeat, HostMessage, ProtocolError,
    ProtocolHeader, ProtocolLimits, Ready, WorkerMessage, FRAME_LENGTH_PREFIX_LEN,
    PROTOCOL_HEADER_LEN, SCHEMA_VERSION,
};

const IPC_FD: RawFd = 3;
const TYPED_DRAW_OPS_CAPABILITY: &str = "typed-draw-ops-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkerOptions {
    limits: ProtocolLimits,
    heartbeat_interval: Duration,
}

#[derive(Debug)]
pub(crate) enum WorkerRuntimeError {
    InvalidArguments,
    Isolation(io::Error),
    Io(io::Error),
    Protocol(ProtocolError),
    Render(RenderSurfaceError),
    InvalidHandshake,
    SessionIdentityMismatch,
    InvalidRenderState,
    InvalidPublishedAck,
    ViewportTooLarge,
    HeartbeatSequenceExhausted,
}

impl std::fmt::Display for WorkerRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerRuntimeError::InvalidArguments => write!(formatter, "invalid worker arguments"),
            WorkerRuntimeError::Isolation(error) => {
                write!(formatter, "worker isolation validation failed: {error}")
            }
            WorkerRuntimeError::Io(error) => write!(formatter, "worker IPC failed: {error}"),
            WorkerRuntimeError::Protocol(error) => {
                write!(formatter, "worker protocol failed: {error}")
            }
            WorkerRuntimeError::Render(error) => write!(formatter, "worker render failed: {error}"),
            WorkerRuntimeError::InvalidHandshake => {
                write!(formatter, "worker rejected the Host handshake")
            }
            WorkerRuntimeError::SessionIdentityMismatch => {
                write!(formatter, "worker rejected a mismatched session identity")
            }
            WorkerRuntimeError::InvalidRenderState => {
                write!(
                    formatter,
                    "worker rejected Render while a frame is outstanding"
                )
            }
            WorkerRuntimeError::InvalidPublishedAck => {
                write!(
                    formatter,
                    "worker rejected a mismatched published acknowledgement"
                )
            }
            WorkerRuntimeError::ViewportTooLarge => {
                write!(
                    formatter,
                    "worker rejected a viewport above the payload limit"
                )
            }
            WorkerRuntimeError::HeartbeatSequenceExhausted => {
                write!(formatter, "worker heartbeat sequence exhausted")
            }
        }
    }
}

impl std::error::Error for WorkerRuntimeError {}

impl From<ProtocolError> for WorkerRuntimeError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<RenderSurfaceError> for WorkerRuntimeError {
    fn from(value: RenderSurfaceError) -> Self {
        Self::Render(value)
    }
}

pub(crate) fn run_from_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(), WorkerRuntimeError> {
    let options = parse_options(args)?;
    verify_detached_context()?;
    let mut ipc = open_ipc_socket()?;
    run_session(&mut ipc, options.limits, options.heartbeat_interval)
}

fn parse_options(
    args: impl IntoIterator<Item = OsString>,
) -> Result<WorkerOptions, WorkerRuntimeError> {
    let mut args = args.into_iter();
    let _program = args.next();
    let mut ipc_fd = None;
    let mut max_payload_bytes = None;
    let mut max_draw_ops = None;
    let mut max_text_bytes = None;
    let mut heartbeat_interval_nanos = None;

    while let Some(argument) = args.next() {
        let argument = argument
            .to_str()
            .ok_or(WorkerRuntimeError::InvalidArguments)?;
        let value = args.next().ok_or(WorkerRuntimeError::InvalidArguments)?;
        let value = value.to_str().ok_or(WorkerRuntimeError::InvalidArguments)?;
        match argument {
            "--ipc-fd" => set_once(&mut ipc_fd, parse_number::<RawFd>(value)?)?,
            "--max-payload-bytes" => set_once(&mut max_payload_bytes, parse_number(value)?)?,
            "--max-draw-ops" => set_once(&mut max_draw_ops, parse_number(value)?)?,
            "--max-text-bytes" => set_once(&mut max_text_bytes, parse_number(value)?)?,
            "--heartbeat-interval-nanos" => {
                set_once(&mut heartbeat_interval_nanos, parse_number::<u64>(value)?)?
            }
            _ => return Err(WorkerRuntimeError::InvalidArguments),
        }
    }

    let ipc_fd = ipc_fd.ok_or(WorkerRuntimeError::InvalidArguments)?;
    let heartbeat_interval =
        Duration::from_nanos(heartbeat_interval_nanos.ok_or(WorkerRuntimeError::InvalidArguments)?);
    if ipc_fd != IPC_FD || heartbeat_interval.is_zero() {
        return Err(WorkerRuntimeError::InvalidArguments);
    }
    Ok(WorkerOptions {
        limits: ProtocolLimits {
            max_payload_bytes: max_payload_bytes.ok_or(WorkerRuntimeError::InvalidArguments)?,
            max_draw_ops: max_draw_ops.ok_or(WorkerRuntimeError::InvalidArguments)?,
            max_text_bytes: max_text_bytes.ok_or(WorkerRuntimeError::InvalidArguments)?,
        },
        heartbeat_interval,
    })
}

fn parse_number<T: std::str::FromStr>(value: &str) -> Result<T, WorkerRuntimeError> {
    value
        .parse::<T>()
        .map_err(|_| WorkerRuntimeError::InvalidArguments)
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), WorkerRuntimeError> {
    if slot.is_some() {
        return Err(WorkerRuntimeError::InvalidArguments);
    }
    *slot = Some(value);
    Ok(())
}

fn open_file_descriptors() -> Result<Vec<RawFd>, WorkerRuntimeError> {
    let entries = fs::read_dir("/proc/self/fd")
        .or_else(|_| fs::read_dir("/dev/fd"))
        .map_err(WorkerRuntimeError::Isolation)?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<RawFd>().ok())
        .collect())
}

fn verify_detached_context() -> Result<(), WorkerRuntimeError> {
    if unsafe { nix::libc::getsid(0) } != unsafe { nix::libc::getpid() } {
        return Err(WorkerRuntimeError::Isolation(io::Error::other(
            "worker is not its session leader",
        )));
    }
    if env::vars_os().next().is_some() || env::current_dir().ok().as_deref() != Some(Path::new("/"))
    {
        return Err(WorkerRuntimeError::Isolation(io::Error::other(
            "ambient environment or cwd remains visible",
        )));
    }
    if OpenOptions::new().read(true).open("/dev/tty").is_ok() {
        return Err(WorkerRuntimeError::Isolation(io::Error::other(
            "worker retained a controlling terminal",
        )));
    }
    for fd in 0..IPC_FD {
        if fs::read_link(format!("/proc/self/fd/{fd}")).map_err(WorkerRuntimeError::Isolation)?
            != Path::new("/dev/null")
        {
            return Err(WorkerRuntimeError::Isolation(io::Error::other(
                "worker standard descriptor is not /dev/null",
            )));
        }
    }
    if unsafe { nix::libc::fcntl(IPC_FD, nix::libc::F_GETFD) } < 0
        || open_file_descriptors()?
            .into_iter()
            .any(|fd| fd > IPC_FD && unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) } >= 0)
    {
        return Err(WorkerRuntimeError::Isolation(io::Error::other(
            "worker descriptor allowlist is invalid",
        )));
    }
    Ok(())
}

fn open_ipc_socket() -> Result<UnixStream, WorkerRuntimeError> {
    let socket = unsafe { UnixStream::from_raw_fd(IPC_FD) };
    socket.peer_addr().map_err(WorkerRuntimeError::Isolation)?;
    Ok(socket)
}

fn run_session(
    ipc: &mut UnixStream,
    limits: ProtocolLimits,
    heartbeat_interval: Duration,
) -> Result<(), WorkerRuntimeError> {
    let Some(frame) = read_frame(ipc, &limits)? else {
        return Ok(());
    };
    let (hello_header, hello) = decode_host_message(&frame, &limits)?;
    let HostMessage::Hello(hello) = hello else {
        return Err(WorkerRuntimeError::InvalidHandshake);
    };
    if hello.protocol_version != SCHEMA_VERSION
        || hello.candidate_version != env!("CARGO_PKG_VERSION")
        || !hello
            .capabilities
            .iter()
            .any(|capability| capability == TYPED_DRAW_OPS_CAPABILITY)
    {
        return Err(WorkerRuntimeError::InvalidHandshake);
    }

    let session_nonce = hello_header.session_nonce;
    let worker_generation = hello_header.worker_generation;
    let ready = WorkerMessage::Ready(Ready {
        protocol_version: SCHEMA_VERSION,
        candidate_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: vec![TYPED_DRAW_OPS_CAPABILITY.to_owned()],
    });
    write_worker_message(
        ipc,
        session_nonce,
        worker_generation,
        hello_header.frame_id,
        &ready,
        &limits,
    )?;

    let mut heartbeat_sequence = 0u64;
    let mut outstanding_frame_id = None;
    let mut latest_acknowledged_frame_id = 0;
    loop {
        if !wait_for_host_message(ipc, heartbeat_interval)? {
            send_heartbeat(
                ipc,
                session_nonce,
                worker_generation,
                latest_acknowledged_frame_id,
                &mut heartbeat_sequence,
                &limits,
            )?;
            continue;
        }
        let Some(frame) = read_frame(ipc, &limits)? else {
            return Ok(());
        };
        let (header, message) = decode_host_message(&frame, &limits)?;
        validate_session_identity(&header, session_nonce, worker_generation)?;
        match message {
            HostMessage::Hello(_) => return Err(WorkerRuntimeError::InvalidHandshake),
            HostMessage::Render(request) => {
                if outstanding_frame_id.is_some() {
                    return Err(WorkerRuntimeError::InvalidRenderState);
                }
                let viewport_area = usize::from(request.viewport.columns)
                    .checked_mul(usize::from(request.viewport.rows))
                    .ok_or(WorkerRuntimeError::ViewportTooLarge)?;
                if viewport_area > limits.max_payload_bytes {
                    return Err(WorkerRuntimeError::ViewportTooLarge);
                }
                let area = Rect::new(0, 0, request.viewport.columns, request.viewport.rows);
                let mut buffer = Buffer::empty(area);
                render_approval_view(&request.render_view, &mut buffer);
                let surface = RenderSurface::new(request.viewport, limits);
                let frame = surface.prepare_frame(
                    &buffer,
                    FrameKey {
                        worker_generation,
                        frame_id: header.frame_id,
                        capture_id: request.capture_id,
                    },
                )?;
                write_worker_message(
                    ipc,
                    session_nonce,
                    worker_generation,
                    header.frame_id,
                    &WorkerMessage::PreparedFrame(frame),
                    &limits,
                )?;
                outstanding_frame_id = Some(header.frame_id);
            }
            HostMessage::PublishedAck(ack) => {
                if ack.frame_id != header.frame_id || outstanding_frame_id != Some(header.frame_id)
                {
                    return Err(WorkerRuntimeError::InvalidPublishedAck);
                }
                outstanding_frame_id = None;
                latest_acknowledged_frame_id = header.frame_id;
                send_heartbeat(
                    ipc,
                    session_nonce,
                    worker_generation,
                    latest_acknowledged_frame_id,
                    &mut heartbeat_sequence,
                    &limits,
                )?;
            }
            HostMessage::Shutdown(_) => return Ok(()),
        }
    }
}

fn wait_for_host_message(
    ipc: &UnixStream,
    heartbeat_interval: Duration,
) -> Result<bool, WorkerRuntimeError> {
    let deadline = Instant::now()
        .checked_add(heartbeat_interval)
        .ok_or(WorkerRuntimeError::InvalidArguments)?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let whole_millis = remaining.as_millis();
        let rounded_millis = whole_millis + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
        let timeout = i32::try_from(rounded_millis.min(i32::MAX as u128))
            .map_err(|_| WorkerRuntimeError::InvalidArguments)?;
        let mut descriptor = nix::libc::pollfd {
            fd: ipc.as_raw_fd(),
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { nix::libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            return Ok(true);
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(WorkerRuntimeError::Io(error));
        }
    }
}

fn send_heartbeat(
    ipc: &mut UnixStream,
    session_nonce: [u8; 16],
    worker_generation: u64,
    frame_id: u64,
    heartbeat_sequence: &mut u64,
    limits: &ProtocolLimits,
) -> Result<(), WorkerRuntimeError> {
    *heartbeat_sequence = heartbeat_sequence
        .checked_add(1)
        .ok_or(WorkerRuntimeError::HeartbeatSequenceExhausted)?;
    write_worker_message(
        ipc,
        session_nonce,
        worker_generation,
        frame_id,
        &WorkerMessage::Heartbeat(Heartbeat {
            monotonic_seq: *heartbeat_sequence,
        }),
        limits,
    )
}

fn validate_session_identity(
    header: &ProtocolHeader,
    session_nonce: [u8; 16],
    worker_generation: u64,
) -> Result<(), WorkerRuntimeError> {
    if header.session_nonce != session_nonce || header.worker_generation != worker_generation {
        return Err(WorkerRuntimeError::SessionIdentityMismatch);
    }
    Ok(())
}

fn read_frame(
    ipc: &mut UnixStream,
    limits: &ProtocolLimits,
) -> Result<Option<Vec<u8>>, WorkerRuntimeError> {
    let mut prefix = [0u8; FRAME_LENGTH_PREFIX_LEN];
    let first = ipc.read(&mut prefix[..1]).map_err(WorkerRuntimeError::Io)?;
    if first == 0 {
        return Ok(None);
    }
    ipc.read_exact(&mut prefix[1..])
        .map_err(WorkerRuntimeError::Io)?;
    let frame_len = u32::from_le_bytes(prefix) as usize;
    if frame_len < PROTOCOL_HEADER_LEN {
        return Err(ProtocolError::FrameTooSmall.into());
    }
    let maximum = PROTOCOL_HEADER_LEN
        .checked_add(limits.max_payload_bytes)
        .ok_or(ProtocolError::FrameTooLarge)?;
    if frame_len > maximum {
        return Err(ProtocolError::FrameTooLarge.into());
    }
    let total_len = FRAME_LENGTH_PREFIX_LEN
        .checked_add(frame_len)
        .ok_or(ProtocolError::FrameTooLarge)?;
    let mut frame = vec![0u8; total_len];
    frame[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(&prefix);
    ipc.read_exact(&mut frame[FRAME_LENGTH_PREFIX_LEN..])
        .map_err(WorkerRuntimeError::Io)?;
    Ok(Some(frame))
}

fn write_worker_message(
    ipc: &mut UnixStream,
    session_nonce: [u8; 16],
    worker_generation: u64,
    frame_id: u64,
    message: &WorkerMessage,
    limits: &ProtocolLimits,
) -> Result<(), WorkerRuntimeError> {
    let frame = encode_worker_message(session_nonce, worker_generation, frame_id, message, limits)?;
    ipc.write_all(&frame).map_err(WorkerRuntimeError::Io)
}
