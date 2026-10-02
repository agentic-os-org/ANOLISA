use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_int, CString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::types::{CommandBlock, ShellEnvironmentSnapshot};

const PATH_DIRECTORY_LIMIT: usize = 32;
const PATH_NAME_LIMIT: usize = 8192;
const SAFE_BASENAME_MAX_BYTES: usize = 128;
const REWRITE_MAX_BYTES: usize = 4096;
const READY_CACHE_ENTRY_LIMIT: usize = 8;
const PENDING_SESSION_LIMIT: usize = 8;
const LOOKUP_DEADLINE: Duration = Duration::from_millis(10);
const DIAGNOSTIC_TAIL_MAX_LINES: usize = 120;
const DIAGNOSTIC_TAIL_MAX_BYTES: usize = 8 * 1024;

const SHELL_BUILTINS: &[&str] = &[
    "alias", "bg", "cd", "command", "dirs", "disown", "echo", "eval", "exec", "exit", "export",
    "false", "fc", "fg", "getopts", "hash", "history", "jobs", "kill", "popd", "printf", "pushd",
    "pwd", "read", "set", "shift", "source", "test", "times", "trap", "true", "type", "typeset",
    "ulimit", "umask", "unalias", "unset", "wait",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CatalogKey {
    session_id: String,
    generation: u64,
}

impl CatalogKey {
    fn new(session_id: &str, generation: u64) -> Self {
        Self {
            session_id: session_id.to_string(),
            generation,
        }
    }
}

#[derive(Debug, Default)]
struct ReadyCatalogState {
    catalogs: HashMap<CatalogKey, CatalogEntry>,
    insertion_order: VecDeque<CatalogKey>,
}

#[derive(Debug)]
enum CatalogEntry {
    Ready(Arc<Vec<String>>),
    Failed,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ReadyCatalogCache {
    state: Arc<Mutex<ReadyCatalogState>>,
}

impl ReadyCatalogCache {
    fn publish(&self, key: CatalogKey, mut names: Vec<String>) {
        names.sort_unstable();
        names.dedup();
        self.publish_entry(key, CatalogEntry::Ready(Arc::new(names)));
    }

    fn publish_failed(&self, key: CatalogKey) {
        self.publish_entry(key, CatalogEntry::Failed);
    }

    fn publish_entry(&self, key: CatalogKey, entry: CatalogEntry) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.catalogs.contains_key(&key) {
            return;
        }
        while state.catalogs.len() >= READY_CACHE_ENTRY_LIMIT {
            let Some(expired) = state.insertion_order.pop_front() else {
                break;
            };
            state.catalogs.remove(&expired);
        }
        state.insertion_order.push_back(key.clone());
        state.catalogs.insert(key, entry);
    }

    fn catalog(&self, session_id: &str, generation: u64) -> Option<Arc<Vec<String>>> {
        self.state
            .try_lock()
            .ok()?
            .catalogs
            .get(&CatalogKey::new(session_id, generation))
            .and_then(|entry| match entry {
                CatalogEntry::Ready(names) => Some(names.clone()),
                CatalogEntry::Failed => None,
            })
    }

    pub(crate) fn contains(&self, session_id: &str, generation: u64) -> bool {
        self.catalog(session_id, generation).is_some()
    }

    fn contains_for_worker(&self, session_id: &str, generation: u64) -> bool {
        self.state.lock().ok().is_some_and(|state| {
            state
                .catalogs
                .contains_key(&CatalogKey::new(session_id, generation))
        })
    }

    #[cfg(test)]
    fn is_processed(&self, session_id: &str, generation: u64) -> bool {
        self.state.lock().ok().is_some_and(|state| {
            state
                .catalogs
                .contains_key(&CatalogKey::new(session_id, generation))
        })
    }

    #[cfg(test)]
    fn publish_for_test<I, S>(&self, session_id: &str, generation: u64, names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.publish(
            CatalogKey::new(session_id, generation),
            names.into_iter().map(Into::into).collect(),
        );
    }
}

pub(crate) trait CatalogScanner: Send + Sync {
    fn scan(&self, snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()>;
}

pub(crate) struct FilesystemCatalogScanner;

impl CatalogScanner for FilesystemCatalogScanner {
    fn scan(&self, snapshot: &ShellEnvironmentSnapshot) -> Result<Vec<String>, ()> {
        if snapshot.path.len() > 8 * 1024 {
            return Err(());
        }
        let mut names = HashSet::new();
        names.extend(SHELL_BUILTINS.iter().map(|name| (*name).to_string()));
        let mut inspected = 0;
        let mut scanned_directory = false;
        for directory in snapshot.path.split(':').take(PATH_DIRECTORY_LIMIT) {
            if directory.is_empty() {
                continue;
            }
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            scanned_directory = true;
            for entry in entries {
                if inspected >= PATH_NAME_LIMIT {
                    break;
                }
                inspected += 1;
                let entry = entry.map_err(|_| ())?;
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !is_safe_basename(&name) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_file() && is_executable_for_current_user(&entry.path()) {
                    names.insert(name);
                }
            }
            if inspected >= PATH_NAME_LIMIT {
                break;
            }
        }
        if !scanned_directory {
            return Err(());
        }
        Ok(names.into_iter().collect())
    }
}

fn is_executable_for_current_user(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // access(X_OK) applies the current process identity and filesystem ACLs.
    unsafe { access(path.as_ptr(), 1) == 0 }
}

unsafe extern "C" {
    fn access(path: *const c_char, mode: c_int) -> c_int;
}

pub(crate) struct ShellRewriteCatalogService {
    cache: ReadyCatalogCache,
    scanner: Arc<dyn CatalogScanner>,
    publisher: Option<ShellRewriteSnapshotPublisher>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ShellRewriteSnapshotPublisher {
    wake: SyncSender<()>,
    pending: Arc<Mutex<HashMap<String, ShellEnvironmentSnapshot>>>,
    shutdown: Arc<AtomicBool>,
    publication_gate: Arc<Mutex<()>>,
}

impl ShellRewriteSnapshotPublisher {
    pub(crate) fn publish(&self, snapshot: ShellEnvironmentSnapshot) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut pending) = self.pending.lock() {
            if !pending.contains_key(&snapshot.session_id) && pending.len() >= PENDING_SESSION_LIMIT
            {
                return;
            }
            match pending.get(&snapshot.session_id) {
                Some(current) if current.marker_sequence > snapshot.marker_sequence => return,
                _ => {
                    pending.insert(snapshot.session_id.clone(), snapshot);
                }
            }
        }
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
        }
    }
}

impl Default for ShellRewriteCatalogService {
    fn default() -> Self {
        Self::with_scanner(Arc::new(FilesystemCatalogScanner))
    }
}

impl ShellRewriteCatalogService {
    pub(crate) fn with_scanner(scanner: Arc<dyn CatalogScanner>) -> Self {
        Self {
            cache: ReadyCatalogCache::default(),
            scanner,
            publisher: None,
            worker: None,
        }
    }

    pub(crate) fn start_worker(&mut self) -> ShellRewriteSnapshotPublisher {
        if let Some(publisher) = &self.publisher {
            return publisher.clone();
        }
        let (wake, receiver) = mpsc::sync_channel(1);
        let cache = self.cache.clone();
        let scanner = self.scanner.clone();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let worker_pending = pending.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let publication_gate = Arc::new(Mutex::new(()));
        let worker_publication_gate = publication_gate.clone();
        self.worker = Some(thread::spawn(move || {
            catalog_worker_loop(
                receiver,
                cache,
                scanner,
                worker_pending,
                worker_shutdown,
                worker_publication_gate,
            )
        }));
        let publisher = ShellRewriteSnapshotPublisher {
            wake,
            pending,
            shutdown,
            publication_gate,
        };
        self.publisher = Some(publisher.clone());
        publisher
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(publisher) = self.publisher.take() {
            publisher.shutdown.store(true, Ordering::Release);
            let _ = publisher.wake.try_send(());
            if let Ok(publication) = publisher.publication_gate.lock() {
                drop(publication);
            }
        }
        if let Some(worker) = self.worker.take() {
            // A PATH entry can block indefinitely in the filesystem; dropping an unfinished
            // handle detaches it, and the shutdown check prevents a late cache publication.
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }

    pub(crate) fn cache(&self) -> ReadyCatalogCache {
        self.cache.clone()
    }

    pub(crate) fn resolve_for_block(
        &self,
        block: &CommandBlock,
        diagnostic_tail: Option<&str>,
    ) -> Option<String> {
        resolve_shell_rewrite_now(
            &self.cache,
            &block.session_id,
            block.shell_environment_generation,
            &block.command,
            diagnostic_tail,
        )
    }

    #[cfg(test)]
    fn wait_until_ready(&self, session_id: &str, generation: u64, timeout: Duration) {
        let started = Instant::now();
        while !self.cache.contains(session_id, generation) && started.elapsed() < timeout {
            thread::yield_now();
        }
    }

    #[cfg(test)]
    fn wait_until_processed(&self, session_id: &str, generation: u64, timeout: Duration) {
        let started = Instant::now();
        while !self.cache.is_processed(session_id, generation) && started.elapsed() < timeout {
            thread::yield_now();
        }
    }
}

impl Drop for ShellRewriteCatalogService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn catalog_worker_loop(
    receiver: mpsc::Receiver<()>,
    cache: ReadyCatalogCache,
    scanner: Arc<dyn CatalogScanner>,
    pending: Arc<Mutex<HashMap<String, ShellEnvironmentSnapshot>>>,
    shutdown: Arc<AtomicBool>,
    publication_gate: Arc<Mutex<()>>,
) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let snapshot = take_pending_snapshot(&pending);
        let snapshot = match snapshot {
            Some(snapshot) => snapshot,
            None => {
                if receiver.recv().is_err() {
                    break;
                }
                continue;
            }
        };
        if cache.contains_for_worker(&snapshot.session_id, snapshot.generation) {
            continue;
        }
        let result = scanner.scan(&snapshot);
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let stale = pending.lock().ok().and_then(|pending| {
            pending.get(&snapshot.session_id).map(|latest| {
                latest.marker_sequence > snapshot.marker_sequence
                    && latest.generation != snapshot.generation
            })
        }) == Some(true);
        if !stale {
            let Ok(_publication) = publication_gate.lock() else {
                break;
            };
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let key = CatalogKey::new(&snapshot.session_id, snapshot.generation);
            match result {
                Ok(names) => cache.publish(key, names),
                Err(()) => cache.publish_failed(key),
            }
        }
    }
}

fn take_pending_snapshot(
    pending: &Mutex<HashMap<String, ShellEnvironmentSnapshot>>,
) -> Option<ShellEnvironmentSnapshot> {
    let mut pending = pending.lock().ok()?;
    let session_id = pending.keys().next()?.clone();
    pending.remove(&session_id)
}

pub(crate) trait DeadlineClock {
    fn elapsed(&self) -> Duration;
}

struct SystemDeadlineClock {
    started: Instant,
}

impl SystemDeadlineClock {
    fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl DeadlineClock for SystemDeadlineClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

pub(crate) fn resolve_shell_rewrite_now(
    cache: &ReadyCatalogCache,
    session_id: &str,
    generation: Option<u64>,
    command: &str,
    diagnostic_tail: Option<&str>,
) -> Option<String> {
    resolve_shell_rewrite(
        cache,
        session_id,
        generation,
        command,
        diagnostic_tail,
        &SystemDeadlineClock::start(),
    )
}

pub(crate) fn resolve_shell_rewrite(
    cache: &ReadyCatalogCache,
    session_id: &str,
    generation: Option<u64>,
    command: &str,
    diagnostic_tail: Option<&str>,
    clock: &dyn DeadlineClock,
) -> Option<String> {
    if clock.elapsed() >= LOOKUP_DEADLINE {
        return None;
    }
    let (program, suffix) = direct_program_and_suffix(command)?;
    let diagnostic_tail = diagnostic_tail?;
    if diagnostic_tail.trim().is_empty()
        || diagnostic_tail.len() > DIAGNOSTIC_TAIL_MAX_BYTES
        || diagnostic_tail.lines().count() > DIAGNOSTIC_TAIL_MAX_LINES
    {
        return None;
    }
    let missing_token = unique_missing_token(diagnostic_tail)?;
    if missing_token != program {
        return None;
    }
    if clock.elapsed() >= LOOKUP_DEADLINE {
        return None;
    }
    let generation = generation?;
    let catalog = cache.catalog(session_id, generation)?;
    if clock.elapsed() >= LOOKUP_DEADLINE {
        return None;
    }
    if catalog.iter().any(|name| name == program) {
        return None;
    }
    let mut candidate = None;
    for (index, name) in catalog.iter().enumerate() {
        if index % 64 == 0 && clock.elapsed() >= LOOKUP_DEADLINE {
            return None;
        }
        if is_wrapper(name) || !is_damerau_levenshtein_one(program, name) {
            continue;
        }
        if candidate.is_some() {
            return None;
        }
        candidate = Some(name.as_str());
    }
    if clock.elapsed() >= LOOKUP_DEADLINE {
        return None;
    }
    let rewritten = format!("{}{}", candidate?, suffix);
    if clock.elapsed() >= LOOKUP_DEADLINE || rewritten.len() > REWRITE_MAX_BYTES {
        return None;
    }
    Some(rewritten)
}

fn unique_missing_token(diagnostic_tail: &str) -> Option<&str> {
    let mut missing_token = None;
    for line in diagnostic_tail.lines() {
        let token = match parse_command_not_found_token(line) {
            Ok(Some(token)) => token,
            Ok(None) => continue,
            Err(()) => return None,
        };
        if missing_token.is_some_and(|current| current != token) {
            return None;
        }
        missing_token = Some(token);
    }
    missing_token
}

fn parse_command_not_found_token(line: &str) -> Result<Option<&str>, ()> {
    let token = if let Some(body) = line.strip_prefix("bash: ") {
        parse_bash_missing_token(body)
    } else if let Some(body) = line.strip_prefix("zsh: command not found: ") {
        (!body.is_empty()).then_some(body)
    } else if let Some(body) = line.strip_prefix("sh: ") {
        parse_sh_missing_token(body)
    } else if line.ends_with(": command not found")
        || line.ends_with(": not found")
        || line.contains(": command not found: ")
    {
        return Err(());
    } else {
        return Ok(None);
    }
    .ok_or(())?;

    is_safe_basename(token).then_some(Some(token)).ok_or(())
}

fn parse_bash_missing_token(body: &str) -> Option<&str> {
    let stem = body.strip_suffix(": command not found")?;
    if let Some(rest) = stem.strip_prefix("line ") {
        let (line_number, token) = rest.split_once(": ")?;
        return (!line_number.is_empty()
            && line_number.bytes().all(|byte| byte.is_ascii_digit())
            && !token.is_empty())
        .then_some(token);
    }
    (!stem.is_empty() && !stem.contains(": ")).then_some(stem)
}

fn parse_sh_missing_token(body: &str) -> Option<&str> {
    let stem = body.strip_suffix(": not found")?;
    if let Some((prefix, token)) = stem.split_once(": ") {
        let line_number = prefix.strip_prefix("line ").unwrap_or(prefix);
        return (!line_number.is_empty()
            && line_number.bytes().all(|byte| byte.is_ascii_digit())
            && !token.is_empty())
        .then_some(token);
    }
    (!stem.is_empty()).then_some(stem)
}

fn direct_program_and_suffix(command: &str) -> Option<(&str, &str)> {
    if command.is_empty()
        || command.len() > REWRITE_MAX_BYTES
        || command.starts_with(' ')
        || command
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == 0x7f || is_shell_metacharacter(byte))
    {
        return None;
    }
    let program_end = command.find(' ').unwrap_or(command.len());
    let program = &command[..program_end];
    if !is_safe_basename(program) || is_wrapper(program) {
        return None;
    }
    Some((program, &command[program_end..]))
}

fn is_shell_metacharacter(byte: u8) -> bool {
    matches!(
        byte,
        b'|' | b'&'
            | b';'
            | b'<'
            | b'>'
            | b'('
            | b')'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'*'
            | b'?'
            | b'!'
            | b'$'
            | b'`'
            | b'\''
            | b'"'
            | b'\\'
            | b'#'
            | b'~'
    )
}

fn is_safe_basename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SAFE_BASENAME_MAX_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
}

fn is_wrapper(program: &str) -> bool {
    matches!(
        program,
        "builtin" | "command" | "env" | "exec" | "nohup" | "ssh" | "sudo" | "time" | "xargs"
    )
}

fn is_damerau_levenshtein_one(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    match left.len().cmp(&right.len()) {
        std::cmp::Ordering::Equal => {
            let differences = left
                .iter()
                .zip(right)
                .enumerate()
                .filter_map(|(index, (left, right))| (left != right).then_some(index))
                .collect::<Vec<_>>();
            differences.len() == 1
                || (differences.len() == 2
                    && differences[1] == differences[0] + 1
                    && left[differences[0]] == right[differences[1]]
                    && left[differences[1]] == right[differences[0]])
        }
        std::cmp::Ordering::Less if right.len() == left.len() + 1 => one_insert_apart(left, right),
        std::cmp::Ordering::Greater if left.len() == right.len() + 1 => {
            one_insert_apart(right, left)
        }
        _ => false,
    }
}

fn one_insert_apart(shorter: &[u8], longer: &[u8]) -> bool {
    let mut short_index = 0;
    let mut long_index = 0;
    let mut skipped = false;
    while short_index < shorter.len() && long_index < longer.len() {
        if shorter[short_index] == longer[long_index] {
            short_index += 1;
            long_index += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
            long_index += 1;
        }
    }
    true
}

#[cfg(test)]
#[path = "shell_rewrite_tests.rs"]
mod tests;
