//! Ordered raw-input event delivery with optional output-relay wakeups.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{SendError, Sender};

use super::RawInputEvent;

pub(super) trait RawInputEventSink {
    fn send(&self, event: RawInputEvent) -> Result<(), SendError<RawInputEvent>>;
}

impl RawInputEventSink for Sender<RawInputEvent> {
    fn send(&self, event: RawInputEvent) -> Result<(), SendError<RawInputEvent>> {
        Sender::send(self, event)
    }
}

pub(super) struct WakingRawInputEventSender {
    sender: Sender<RawInputEvent>,
    wake: Option<UnixStream>,
}

impl WakingRawInputEventSender {
    pub(super) fn new(sender: Sender<RawInputEvent>, wake: Option<UnixStream>) -> Self {
        Self { sender, wake }
    }

    pub(super) fn notify_relay(&self) {
        let Some(wake) = self.wake.as_ref() else {
            return;
        };
        let mut wake = wake;
        match wake.write(&[1]) {
            Ok(_) => {}
            // A full socket already represents a pending wakeup.
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
    }
}

impl RawInputEventSink for WakingRawInputEventSender {
    fn send(&self, event: RawInputEvent) -> Result<(), SendError<RawInputEvent>> {
        self.sender.send(event)?;
        self.notify_relay();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{RawInputEventSink, WakingRawInputEventSender};
    use crate::raw_input::RawInputEvent;

    #[test]
    fn event_send_publishes_to_the_final_channel_and_wakes_the_relay() {
        let (mut wake_reader, wake_writer) = super::UnixStream::pair().expect("wake pair");
        wake_reader
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("wake timeout");
        let (sender, receiver) = mpsc::channel();
        let sender = WakingRawInputEventSender::new(sender, Some(wake_writer));

        sender.send(RawInputEvent::CtrlC).expect("send event");

        assert_eq!(
            receiver.try_recv().expect("published event"),
            RawInputEvent::CtrlC
        );
        let mut wake = [0_u8; 1];
        wake_reader.read_exact(&mut wake).expect("relay wake");
    }
}
