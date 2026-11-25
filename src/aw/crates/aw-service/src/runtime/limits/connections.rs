//! Ordinary RPCs hold capacity through execution and delivery of their reply.

use crate::{ipc, rejected, Result};
use std::{
    collections::BTreeMap,
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    time::Instant,
};

#[derive(PartialEq, Eq)]
enum Phase {
    // Reading the frame or preparing an SDK callback before it can execute.
    Pending,
    Handling,
    Callback {
        instance: String,
        correlation: String,
    },
    Replying,
}

pub(crate) enum Admission {
    Ordinary,
    SdkCallback,
    Control,
}

type Sockets = BTreeMap<RawFd, Phase>;

fn peer_fd(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: 0,
        revents: 0,
    }
}

fn poll_hangups(peers: &mut [libc::pollfd]) -> io::Result<()> {
    // SAFETY: the registry lock keeps the descriptors open for this poll.
    if unsafe { libc::poll(peers.as_mut_ptr(), peers.len() as _, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// The caller holds the registry lock, so a pending worker cannot start
// execution between the hangup check and removal of its slot.
fn reclaim_disconnected(sockets: &mut Sockets) -> io::Result<()> {
    let mut peers: Vec<_> = sockets
        .iter()
        .filter(|(_, phase)| matches!(phase, Phase::Pending | Phase::Replying))
        .map(|(&fd, _)| peer_fd(fd))
        .collect();
    poll_hangups(&mut peers)?;
    for peer in peers {
        // POLLHUP excludes write-half-closed clients still waiting for a reply.
        if peer.revents & libc::POLLHUP != 0 {
            sockets.remove(&peer.fd);
        }
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct Connections {
    sockets: Mutex<Sockets>,
    callbacks_changed: Condvar,
}

impl Connections {
    fn lock(&self) -> io::Result<MutexGuard<'_, Sockets>> {
        self.sockets
            .lock()
            .map_err(|_| io::Error::other("connection admission poisoned"))
    }

    pub(crate) fn track(self: &Arc<Self>, stream: UnixStream) -> io::Result<Connection> {
        self.lock()?.insert(stream.as_raw_fd(), Phase::Pending);
        Ok(Connection {
            stream,
            connections: Arc::clone(self),
        })
    }

    pub(in crate::runtime) fn wait_for_callback_replies(
        &self,
        instance: &str,
        correlation: String,
        deadline: Instant,
    ) -> Result<()> {
        let callback = Phase::Callback {
            instance: instance.into(),
            correlation,
        };
        let mut sockets = self.lock()?;
        reclaim_disconnected(&mut sockets)?;
        let (_sockets, timeout) = self
            .callbacks_changed
            .wait_timeout_while(
                sockets,
                deadline.saturating_duration_since(Instant::now()),
                |sockets| sockets.values().any(|phase| phase == &callback),
            )
            .map_err(|_| rejected("state_poisoned"))?;
        if timeout.timed_out() {
            return Err(rejected("event_close_timeout"));
        }
        Ok(())
    }

    pub(super) fn count(&self) -> io::Result<usize> {
        Ok(self.lock()?.len())
    }
}

pub(crate) struct Connection {
    stream: UnixStream,
    connections: Arc<Connections>,
}

impl Connection {
    pub(crate) fn stream(&self) -> &UnixStream {
        &self.stream
    }

    pub(crate) fn admit(&self, kind: Admission, limit: usize) -> Result<bool> {
        let mut sockets = self.connections.lock()?;
        let fd = self.stream.as_raw_fd();
        if matches!(kind, Admission::Control) {
            // A submitted cleanup request can outlive its reader.
            sockets.insert(fd, Phase::Handling);
            return Ok(true);
        }
        if sockets.len() > limit {
            reclaim_disconnected(&mut sockets)?;
        }
        let within_limit = sockets.len() <= limit;
        let phase = sockets
            .get_mut(&fd)
            .ok_or_else(|| rejected("event_cancelled"))?;
        if within_limit && matches!(kind, Admission::Ordinary) {
            *phase = Phase::Handling;
        }
        Ok(within_limit)
    }

    pub(in crate::runtime) fn start_callback(
        &self,
        instance: &str,
        correlation: Option<String>,
    ) -> Result<()> {
        let mut sockets = self.connections.lock()?;
        // The client can disconnect after an explicit close or before a new
        // admission gets a chance to reclaim this still-pending connection.
        let mut peer = peer_fd(self.stream.as_raw_fd());
        poll_hangups(std::slice::from_mut(&mut peer))?;
        if peer.revents & libc::POLLHUP != 0 {
            sockets.remove(&peer.fd);
            return Err(rejected("event_cancelled"));
        }
        let slot = sockets
            .get_mut(&self.stream.as_raw_fd())
            .ok_or_else(|| rejected("event_cancelled"))?;
        *slot = match correlation {
            Some(key) => Phase::Callback {
                instance: instance.into(),
                correlation: key,
            },
            None => Phase::Handling,
        };
        Ok(())
    }

    pub(crate) fn begin_reply(&self) -> io::Result<()> {
        let mut sockets = self.connections.lock()?;
        let Some(phase) = sockets.get_mut(&self.stream.as_raw_fd()) else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let previous = std::mem::replace(phase, Phase::Replying);
        if matches!(previous, Phase::Callback { .. }) {
            self.connections.callbacks_changed.notify_all();
        }
        Ok(())
    }

    pub(crate) fn send_reply(&self, pending: &[u8]) -> io::Result<usize> {
        // Admission must observe the release once the reader sees the last byte.
        let mut sockets = self.connections.lock()?;
        let sent = ipc::try_send(&self.stream, pending)?;
        if sent == pending.len() {
            sockets.remove(&self.stream.as_raw_fd());
        }
        Ok(sent)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Remove the borrowed descriptor before dropping its owning stream.
        let previous = self
            .connections
            .sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.stream.as_raw_fd());
        if matches!(previous, Some(Phase::Callback { .. })) {
            self.connections.callbacks_changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;

    #[test]
    fn disconnected_worker_holds_capacity_until_execution_finishes() {
        let slots = Arc::new(Connections::default());
        let (old, peer) = UnixStream::pair().unwrap();
        let old = slots.track(old).unwrap();
        assert!(old.admit(Admission::Ordinary, 1).unwrap());

        let (fresh, _fresh_peer) = UnixStream::pair().unwrap();
        let fresh = slots.track(fresh).unwrap();
        assert!(!fresh.admit(Admission::Ordinary, 1).unwrap());
        drop(peer);
        assert!(!fresh.admit(Admission::Ordinary, 1).unwrap());
        old.begin_reply().unwrap();
        assert!(fresh.admit(Admission::Ordinary, 1).unwrap());
        assert_eq!(slots.count().unwrap(), 1);
        drop(old);
        assert_eq!(slots.count().unwrap(), 1);
    }

    #[test]
    fn pending_callback_requires_a_reader_before_it_can_run() {
        let slots = Arc::new(Connections::default());
        let (stream, peer) = UnixStream::pair().unwrap();
        let old = slots.track(stream).unwrap();
        assert!(old.admit(Admission::SdkCallback, 1).unwrap());
        drop(peer);
        assert!(old.start_callback("scope", Some("call".into())).is_err());
        assert_eq!(slots.count().unwrap(), 0);

        let (stream, peer) = UnixStream::pair().unwrap();
        let next = slots.track(stream).unwrap();
        assert!(next.admit(Admission::SdkCallback, 1).unwrap());
        peer.shutdown(Shutdown::Write).unwrap();
        next.start_callback("scope", Some("next".into())).unwrap();
        assert_eq!(slots.count().unwrap(), 1);
    }

    #[test]
    fn half_closed_reader_keeps_its_reply_slot() {
        let slots = Arc::new(Connections::default());
        let (old, peer) = UnixStream::pair().unwrap();
        let old = slots.track(old).unwrap();
        assert!(old.admit(Admission::Ordinary, 1).unwrap());
        old.begin_reply().unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        let (fresh, _peer) = UnixStream::pair().unwrap();
        let fresh = slots.track(fresh).unwrap();
        assert!(!fresh.admit(Admission::Ordinary, 1).unwrap());
        assert_eq!(old.send_reply(b"done").unwrap(), 4);
        assert!(fresh.admit(Admission::Ordinary, 1).unwrap());
    }
}
