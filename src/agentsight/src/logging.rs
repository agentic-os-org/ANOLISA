//! Process-global logger with swappable output for repeated `init_logging` calls.
//!
//! `env_logger::try_init()` only succeeds once per process. AgentSight may call
//! `init_logging` on every `AgentSight::new` (FFI new+start cycles), so we
//! install a custom `log::Log` once and reconfigure its filter + writer on later calls.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

use env_filter::Filter;
use log::{LevelFilter, Log, Metadata, Record};

static LOGGER: OnceLock<AgentsightLogger> = OnceLock::new();

/// Initialize or reconfigure process logging.
///
/// * `verbose` — true = debug, false = warn (unless `RUST_LOG` is set)
/// * `log_path` — append to this file when set; otherwise stderr
pub fn init(verbose: bool, log_path: Option<&str>) {
    let filter = build_filter(verbose);
    let writer = open_log_writer(log_path);

    if let Some(logger) = LOGGER.get() {
        logger.reconfigure(filter, writer);
        return;
    }

    let max_level = filter.filter();
    let logger = AgentsightLogger::new(filter, writer);
    if LOGGER.set(logger).is_ok() {
        log::set_max_level(max_level);
        if let Err(err) = log::set_logger(LOGGER.get().expect("logger just set")) {
            eprintln!("agentsight: failed to install logger: {err}");
        }
    } else if let Some(logger) = LOGGER.get() {
        // Lost a concurrent first-time install race; apply this caller's config.
        logger.reconfigure(build_filter(verbose), open_log_writer(log_path));
    }
}

fn build_filter(verbose: bool) -> Filter {
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        let mut builder = env_filter::Builder::new();
        match builder.try_parse(&rust_log) {
            Ok(_) => builder.build(),
            Err(e) => {
                eprintln!("agentsight: invalid RUST_LOG={rust_log:?}: {e}");
                default_filter(verbose)
            }
        }
    } else {
        default_filter(verbose)
    }
}

fn default_filter(verbose: bool) -> Filter {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Warn
    };
    env_filter::Builder::new().filter_level(level).build()
}

fn open_log_writer(log_path: Option<&str>) -> LogWriter {
    let Some(path) = log_path else {
        return LogWriter::Stderr;
    };

    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => LogWriter::File(file),
        Err(e) => {
            eprintln!("agentsight: failed to open log file {path:?}: {e}");
            LogWriter::Stderr
        }
    }
}

enum LogWriter {
    Stderr,
    File(File),
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            LogWriter::Stderr => io::stderr().write(buf),
            LogWriter::File(file) => file.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            LogWriter::Stderr => io::stderr().flush(),
            LogWriter::File(file) => file.flush(),
        }
    }
}

struct AgentsightLogger {
    filter: Mutex<Filter>,
    writer: Mutex<LogWriter>,
}

impl AgentsightLogger {
    fn new(filter: Filter, writer: LogWriter) -> Self {
        Self {
            filter: Mutex::new(filter),
            writer: Mutex::new(writer),
        }
    }

    fn reconfigure(&self, filter: Filter, writer: LogWriter) {
        log::set_max_level(filter.filter());
        *self.filter.lock().unwrap_or_else(|e| e.into_inner()) = filter;
        *self.writer.lock().unwrap_or_else(|e| e.into_inner()) = writer;
    }
}

impl Log for AgentsightLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.filter
            .lock()
            .expect("log filter lock poisoned")
            .enabled(metadata)
    }

    fn log(&self, record: &Record) {
        // `matches` (not just `enabled`) so a `RUST_LOG=level/regex` message
        // filter is honored.
        if !self
            .filter
            .lock()
            .expect("log filter lock poisoned")
            .matches(record)
        {
            return;
        }

        let line = format_record(record);
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(writer, "{line}");
    }

    fn flush(&self) {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writer.flush();
    }
}

fn format_record(record: &Record) -> String {
    use std::fmt::Write as _;

    let mut line = String::new();
    let _ = write!(
        line,
        "[{} {:5}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        record.level(),
    );
    if let Some(path) = record.module_path() {
        let _ = write!(line, " {path}");
    }
    if let Some(line_no) = record.line() {
        let _ = write!(line, ":{line_no}");
    }
    let _ = write!(line, "] {}", record.args());
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn build_filter_respects_verbose_default() {
        let filter = default_filter(false);
        assert_eq!(filter.filter(), LevelFilter::Warn);

        let filter = default_filter(true);
        assert_eq!(filter.filter(), LevelFilter::Debug);
    }

    #[test]
    fn log_writer_file_append_and_write() {
        let dir = std::env::temp_dir().join(format!("agentsight-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("test.log");

        {
            let mut writer = open_log_writer(Some(path.to_str().unwrap()));
            writeln!(writer, "first").expect("write");
        }
        {
            let mut writer = open_log_writer(Some(path.to_str().unwrap()));
            writeln!(writer, "second").expect("write");
        }

        let mut contents = String::new();
        File::open(&path)
            .expect("open log")
            .read_to_string(&mut contents)
            .expect("read log");
        assert!(contents.contains("first"));
        assert!(contents.contains("second"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After intentionally poisoning filter and writer mutexes,
    /// reconfigure / log / flush should still operate via poison recovery.
    ///
    /// NOTE: `enabled()` uses `expect()`, not `unwrap_or_else`, so calling
    /// `log()` with a poisoned filter would panic. We poison the writer first,
    /// exercise it through log+flush (filter stays healthy so enabled() works),
    /// then poison the filter separately to exercise reconfigure's filter path.
    #[test]
    fn poison_recovery_filter_and_writer_still_operational() {
        let logger = AgentsightLogger::new(default_filter(true), open_log_writer(None));

        // Poison only the writer mutex (filter stays healthy so enabled() works)
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _w = logger.writer.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err(), "Writer mutex should be poisoned");

        // log() calls enabled() (healthy filter) then recovers poisoned writer
        // via writer.lock().unwrap_or_else(|e| e.into_inner())
        logger.log(
            &log::Record::builder()
                .args(format_args!("poison-recovery test"))
                .level(log::Level::Info)
                .target("test")
                .build(),
        );

        // flush() uses writer.lock().unwrap_or_else(|e| e.into_inner())
        logger.flush();

        // Now poison the filter to exercise reconfigure's filter poison path
        let result2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _f = logger.filter.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result2.is_err(), "Filter mutex should be poisoned");

        // reconfigure() uses unwrap_or_else for both filter and writer locks
        logger.reconfigure(default_filter(false), open_log_writer(None));
        // Call again — both are still poisoned (poison flag persists), so
        // both unwrap_or_else paths are exercised a second time
        logger.reconfigure(default_filter(true), open_log_writer(None));
    }

    /// `RUST_LOG=level/regex` must filter on the formatted message, not only
    /// on record metadata.
    #[test]
    fn log_applies_message_regex_filter() {
        let mut builder = env_filter::Builder::new();
        builder
            .try_parse("info/listening")
            .expect("fixture filter should parse");
        let filter = builder.build();

        let dir = std::env::temp_dir().join(format!("agentsight-log-regex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("regex.log");

        let logger = AgentsightLogger::new(filter, open_log_writer(path.to_str()));
        for message in [
            "agentsight-enforcer listening on socket",
            "unrelated chatter",
        ] {
            logger.log(
                &Record::builder()
                    .args(format_args!("{message}"))
                    .level(log::Level::Info)
                    .target("test")
                    .build(),
            );
        }
        logger.flush();

        let contents = std::fs::read_to_string(&path).expect("read log");
        assert!(contents.contains("listening on socket"));
        assert!(!contents.contains("unrelated chatter"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_reconfigure_swaps_writer() {
        let dir =
            std::env::temp_dir().join(format!("agentsight-log-reconfig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path_a = dir.join("a.log");
        let path_b = dir.join("b.log");

        let logger = AgentsightLogger::new(default_filter(true), open_log_writer(None));
        logger.log(
            &Record::builder()
                .args(format_args!("to stderr"))
                .level(log::Level::Info)
                .target("test")
                .build(),
        );

        logger.reconfigure(
            default_filter(true),
            open_log_writer(Some(path_a.to_str().unwrap())),
        );
        logger.log(
            &Record::builder()
                .args(format_args!("to a"))
                .level(log::Level::Info)
                .target("test")
                .build(),
        );

        logger.reconfigure(
            default_filter(true),
            open_log_writer(Some(path_b.to_str().unwrap())),
        );
        logger.log(
            &Record::builder()
                .args(format_args!("to b"))
                .level(log::Level::Info)
                .target("test")
                .build(),
        );

        let a = std::fs::read_to_string(&path_a).expect("read a");
        assert!(a.contains("to a"));
        assert!(!a.contains("to b"));

        let b = std::fs::read_to_string(&path_b).expect("read b");
        assert!(b.contains("to b"));
        assert!(!b.contains("to a"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
