//! Bounded, lossy diagnostics. Only the worker may block on the output sink.
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};
use tracing_subscriber::fmt::MakeWriter;

const QUEUE_RECORDS: usize = 64;
const RECORD_BYTES: usize = 32 * 1024;

#[derive(Clone, Default)]
pub(super) struct DiagnosticWriter {
    sender: Option<SyncSender<Vec<u8>>>,
    stopping: Arc<AtomicBool>,
}
pub(super) struct LogWorker {
    writer: DiagnosticWriter,
    done: Receiver<()>,
}
impl DiagnosticWriter {
    pub fn start(mut sink: impl Write + Send + 'static) -> io::Result<(Self, LogWorker)> {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(QUEUE_RECORDS);
        let (finished, done) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        std::thread::Builder::new()
            .name("asc-diagnostics".into())
            .spawn(move || {
                loop {
                    if stop.load(Ordering::Acquire) {
                        for record in receiver.try_iter() {
                            let _ = sink.write_all(&record);
                        }
                        break;
                    }
                    match receiver.recv_timeout(Duration::from_millis(5)) {
                        Ok(record) => {
                            let _ = sink.write_all(&record);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = sink.flush();
                let _ = finished.send(());
            })?;
        let writer = Self {
            sender: Some(sender),
            stopping,
        };
        Ok((writer.clone(), LogWorker { writer, done }))
    }
    pub fn emit(&self, record: Vec<u8>) {
        if !record.is_empty()
            && record.len() <= RECORD_BYTES
            && !self.stopping.load(Ordering::Acquire)
            && let Some(sender) = &self.sender
        {
            let _ = sender.try_send(record);
        }
    }
}
impl LogWorker {
    pub fn shutdown(self, timeout: Duration) {
        self.writer.stopping.store(true, Ordering::Release);
        let _ = self.done.recv_timeout(timeout);
        // Never join a worker which may be blocked in an OS write. Process exit
        // terminates it; diagnostics must not extend the caller's shutdown budget.
    }
}
impl Drop for LogWorker {
    fn drop(&mut self) {
        self.writer.stopping.store(true, Ordering::Release);
    }
}

pub(super) struct RecordWriter {
    output: DiagnosticWriter,
    bytes: Vec<u8>,
    oversized: bool,
}
impl<'a> MakeWriter<'a> for DiagnosticWriter {
    type Writer = RecordWriter;
    fn make_writer(&'a self) -> Self::Writer {
        RecordWriter {
            output: self.clone(),
            bytes: Vec::new(),
            oversized: false,
        }
    }
}
impl Write for RecordWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > RECORD_BYTES.saturating_sub(self.bytes.len()) {
            self.oversized = true;
        }
        if !self.oversized {
            self.bytes.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for RecordWriter {
    fn drop(&mut self) {
        if !self.oversized {
            self.output.emit(std::mem::take(&mut self.bytes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct BlockedSink {
        entered: mpsc::Sender<()>,
        release: Receiver<()>,
        output: mpsc::Sender<Vec<u8>>,
    }
    impl Write for BlockedSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            self.output.send(bytes.to_vec()).unwrap();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn blocked_sink_drops_overflow_and_shutdown_does_not_wait_for_io() {
        let (entered, wait) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (output, records) = mpsc::channel();
        let (writer, worker) = DiagnosticWriter::start(BlockedSink {
            entered,
            release: resume,
            output,
        })
        .unwrap();
        writer.emit(b"first".to_vec());
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..QUEUE_RECORDS + 10 {
            writer.emit(b"queued".to_vec());
        }
        worker.shutdown(Duration::ZERO);
        for _ in 0..=QUEUE_RECORDS {
            release.send(()).unwrap();
        }
        drop(writer);
        let captured: Vec<_> = (0..=QUEUE_RECORDS)
            .map(|_| records.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        assert_eq!(captured[0], b"first");
        assert!(captured[1..].iter().all(|record| record == b"queued"));
        assert!(records.recv_timeout(Duration::from_secs(2)).is_err());
    }
    #[test]
    fn oversized_records_are_dropped_whole() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let writer = DiagnosticWriter {
            sender: Some(sender),
            ..DiagnosticWriter::default()
        };
        {
            let mut record = writer.make_writer();
            record.write_all(&vec![b'x'; RECORD_BYTES]).unwrap();
            record.write_all(b"overflow").unwrap();
        }
        assert!(receiver.try_recv().is_err());
        {
            let mut record = writer.make_writer();
            record.write_all(b"complete\n").unwrap();
        }
        assert_eq!(receiver.try_recv().unwrap(), b"complete\n");
    }
}
