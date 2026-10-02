use crate::probes::filewatch::FileWatchEvent;
use crate::probes::filewrite::FileWriteEvent;
use crate::probes::procmon::Event as ProcMonEvent;
use crate::probes::proctrace::VariableEvent as ProcEvent;
use crate::probes::sslsniff::SslEvent;
use crate::probes::udpdns::UdpDnsEvent;

/// Unified event type that can represent any probe event
///
/// Both variants are lightweight and can be stored directly.
#[derive(Debug)]
pub enum Event {
    Ssl(SslEvent),
    Proc(ProcEvent),
    ProcMon(ProcMonEvent),
    FileWatch(FileWatchEvent),
    FileWrite(FileWriteEvent),
    UdpDns(UdpDnsEvent),
}

impl Event {
    /// Get the message type name for logging/debugging
    pub fn event_type(&self) -> &'static str {
        match self {
            Event::Ssl(_) => "Ssl",
            Event::Proc(_) => "Proc",
            Event::ProcMon(_) => "ProcMon",
            Event::FileWatch(_) => "FileWatch",
            Event::FileWrite(_) => "FileWrite",
            Event::UdpDns(_) => "UdpDns",
        }
    }

    /// Approximate memory footprint of this event, in bytes.
    ///
    /// Counts the enum itself plus the variable-length payloads it owns, which
    /// is what makes events differ by four orders of magnitude: an SSL record
    /// carries anywhere from a few bytes to 4 MiB. The probe event channel uses
    /// this to bound in-flight memory, something its slot count cannot do
    /// (#2888). Approximate by design — allocator overhead and `String`
    /// capacity beyond `len()` are not tracked, so it under-reports slightly
    /// and the budget should be set with headroom.
    pub fn approx_bytes(&self) -> usize {
        let payload = match self {
            Event::Ssl(e) => e.comm.len() + e.buf.len(),
            Event::Proc(e) => match e {
                ProcEvent::Exec { filename, args, .. } => filename.len() + args.len(),
                ProcEvent::Stdout { payload, .. } => payload.len(),
                ProcEvent::Exit { .. } | ProcEvent::Unknown(_) => 0,
            },
            Event::ProcMon(e) => match e {
                ProcMonEvent::Exec { comm, .. } | ProcMonEvent::Exit { comm, .. } => comm.len(),
            },
            Event::FileWatch(e) => e.comm.len() + e.filename.len(),
            Event::FileWrite(e) => e.comm.len() + e.filename.len() + e.buf.len(),
            Event::UdpDns(e) => e.comm.len() + e.domain.len(),
        };
        std::mem::size_of::<Self>() + payload
    }
}

impl Event {
    /// Check if this is an SSL event
    pub fn is_ssl(&self) -> bool {
        matches!(self, Event::Ssl(_))
    }

    /// Check if this is a process event
    pub fn is_proc(&self) -> bool {
        matches!(self, Event::Proc(_))
    }

    /// Check if this is a procmon event
    pub fn is_procmon(&self) -> bool {
        matches!(self, Event::ProcMon(_))
    }

    /// Check if this is a file watch event
    pub fn is_filewatch(&self) -> bool {
        matches!(self, Event::FileWatch(_))
    }

    /// Check if this is a file write event
    pub fn is_filewrite(&self) -> bool {
        matches!(self, Event::FileWrite(_))
    }

    /// Get SSL event if this is one
    pub fn as_ssl(&self) -> Option<&SslEvent> {
        match self {
            Event::Ssl(e) => Some(e),
            _ => None,
        }
    }

    /// Get process event if this is one
    pub fn as_proc(&self) -> Option<&ProcEvent> {
        match self {
            Event::Proc(e) => Some(e),
            _ => None,
        }
    }

    /// Get procmon event if this is one
    pub fn as_procmon(&self) -> Option<&ProcMonEvent> {
        match self {
            Event::ProcMon(e) => Some(e),
            _ => None,
        }
    }

    /// Get file watch event if this is one
    pub fn as_filewatch(&self) -> Option<&FileWatchEvent> {
        match self {
            Event::FileWatch(e) => Some(e),
            _ => None,
        }
    }

    /// Get file write event if this is one
    pub fn as_filewrite(&self) -> Option<&FileWriteEvent> {
        match self {
            Event::FileWrite(e) => Some(e),
            _ => None,
        }
    }

    /// Check if this is a UDP DNS event
    pub fn is_udpdns(&self) -> bool {
        matches!(self, Event::UdpDns(_))
    }

    /// Get UDP DNS event if this is one
    pub fn as_udpdns(&self) -> Option<&UdpDnsEvent> {
        match self {
            Event::UdpDns(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ssl_event() -> SslEvent {
        SslEvent {
            source: 0,
            timestamp_ns: 100,
            delta_ns: 0,
            pid: 1234,
            tid: 1234,
            uid: 1000,
            len: 5,
            rw: 0,
            comm: "test".to_string(),
            buf: b"hello".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x1000,
        }
    }

    fn make_filewrite_event() -> FileWriteEvent {
        FileWriteEvent {
            pid: 5678,
            tid: 5678,
            uid: 1000,
            timestamp_ns: 200,
            write_size: 10,
            comm: "writer".to_string(),
            filename: "test.jsonl".to_string(),
            cgroup_id: 0,
            buf: b"content".to_vec(),
        }
    }

    fn make_filewatch_event() -> FileWatchEvent {
        FileWatchEvent {
            pid: 9999,
            tid: 9999,
            uid: 0,
            timestamp_ns: 300,
            flags: 0,
            comm: "watcher".to_string(),
            filename: "data.jsonl".to_string(),
            cgroup_id: 0,
        }
    }

    #[test]
    fn test_event_type_ssl() {
        let e = Event::Ssl(make_ssl_event());
        assert_eq!(e.event_type(), "Ssl");
    }

    #[test]
    fn test_event_type_filewrite() {
        let e = Event::FileWrite(make_filewrite_event());
        assert_eq!(e.event_type(), "FileWrite");
    }

    #[test]
    fn test_event_type_filewatch() {
        let e = Event::FileWatch(make_filewatch_event());
        assert_eq!(e.event_type(), "FileWatch");
    }

    #[test]
    fn test_is_ssl() {
        let e = Event::Ssl(make_ssl_event());
        assert!(e.is_ssl());
        assert!(!e.is_proc());
        assert!(!e.is_procmon());
        assert!(!e.is_filewatch());
        assert!(!e.is_filewrite());
    }

    #[test]
    fn test_is_filewrite() {
        let e = Event::FileWrite(make_filewrite_event());
        assert!(e.is_filewrite());
        assert!(!e.is_ssl());
    }

    #[test]
    fn test_is_filewatch() {
        let e = Event::FileWatch(make_filewatch_event());
        assert!(e.is_filewatch());
        assert!(!e.is_ssl());
    }

    #[test]
    fn test_as_ssl_some() {
        let e = Event::Ssl(make_ssl_event());
        let ssl = e.as_ssl().unwrap();
        assert_eq!(ssl.pid, 1234);
    }

    #[test]
    fn test_as_ssl_none() {
        let e = Event::FileWrite(make_filewrite_event());
        assert!(e.as_ssl().is_none());
    }

    #[test]
    fn test_as_filewrite_some() {
        let e = Event::FileWrite(make_filewrite_event());
        let fw = e.as_filewrite().unwrap();
        assert_eq!(fw.pid, 5678);
    }

    #[test]
    fn test_as_filewatch_some() {
        let e = Event::FileWatch(make_filewatch_event());
        let fw = e.as_filewatch().unwrap();
        assert_eq!(fw.pid, 9999);
    }

    #[test]
    fn test_as_proc_none_for_ssl() {
        let e = Event::Ssl(make_ssl_event());
        assert!(e.as_proc().is_none());
        assert!(e.as_procmon().is_none());
        assert!(e.as_filewatch().is_none());
        assert!(e.as_filewrite().is_none());
    }

    #[test]
    fn approx_bytes_tracks_payload_not_just_the_enum() {
        let small = Event::Ssl(make_ssl_event());
        let mut big_event = make_ssl_event();
        big_event.buf = vec![0u8; 4 * 1024 * 1024];
        let big = Event::Ssl(big_event);

        // Two events occupying the same channel slot differ by ~4 MiB: exactly
        // why the slot count cannot bound in-flight memory (#2888).
        let baseline = std::mem::size_of::<Event>() + "test".len();
        assert_eq!(small.approx_bytes(), baseline + "hello".len());
        assert_eq!(big.approx_bytes(), baseline + 4 * 1024 * 1024);
        assert!(big.approx_bytes() > small.approx_bytes() * 1000);
    }

    #[test]
    fn approx_bytes_counts_every_variant_payload() {
        /// A zeroed header stands in for the fixed part of a proc event.
        ///
        /// SAFETY: `proc_event_header` is a `#[repr(C)]` bindgen struct of plain
        /// integers, so an all-zero bit pattern is a valid value. `approx_bytes`
        /// reads none of its fields — only the variable-length ones alongside it.
        fn proc_header() -> crate::probes::proctrace::ProcEventHeader {
            unsafe { std::mem::zeroed() }
        }
        // A variant whose payload is ignored would silently escape the budget, so
        // every one of them is checked against its own payload size.
        let baseline = std::mem::size_of::<Event>();

        let filewrite = Event::FileWrite(make_filewrite_event());
        assert_eq!(
            filewrite.approx_bytes(),
            baseline + "writer".len() + "test.jsonl".len() + "content".len()
        );

        let filewatch = Event::FileWatch(make_filewatch_event());
        assert_eq!(
            filewatch.approx_bytes(),
            baseline + "watcher".len() + "data.jsonl".len()
        );

        let exec = Event::Proc(ProcEvent::Exec {
            header: proc_header(),
            filename: "/usr/bin/node".to_string(),
            args: "node index.js".to_string(),
        });
        assert_eq!(
            exec.approx_bytes(),
            baseline + "/usr/bin/node".len() + "node index.js".len()
        );

        let stdout = Event::Proc(ProcEvent::Stdout {
            header: proc_header(),
            fd: 1,
            payload: vec![0u8; 128],
        });
        assert_eq!(stdout.approx_bytes(), baseline + 128);

        // Fixed-size variants carry no payload beyond the enum itself.
        let exit = Event::Proc(ProcEvent::Exit {
            header: proc_header(),
            exit_code: 0,
        });
        assert_eq!(exit.approx_bytes(), baseline);
        assert_eq!(Event::Proc(ProcEvent::Unknown(7)).approx_bytes(), baseline);

        let procmon_exec = Event::ProcMon(ProcMonEvent::Exec {
            pid: 1,
            tid: 1,
            ppid: 0,
            uid: 0,
            timestamp_ns: 1,
            comm: "claude".to_string(),
        });
        assert_eq!(procmon_exec.approx_bytes(), baseline + "claude".len());

        let procmon_exit = Event::ProcMon(ProcMonEvent::Exit {
            pid: 1,
            tid: 1,
            uid: 0,
            timestamp_ns: 2,
            comm: "claude".to_string(),
            exit_code: 0,
        });
        assert_eq!(procmon_exit.approx_bytes(), baseline + "claude".len());

        let dns = Event::UdpDns(UdpDnsEvent {
            pid: 1,
            tid: 1,
            uid: 0,
            timestamp_ns: 3,
            comm: "curl".to_string(),
            domain: "api.anthropic.com".to_string(),
        });
        assert_eq!(
            dns.approx_bytes(),
            baseline + "curl".len() + "api.anthropic.com".len()
        );
    }
}
