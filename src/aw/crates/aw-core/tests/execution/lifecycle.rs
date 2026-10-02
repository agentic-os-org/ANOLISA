//! Execution releases only its own writer on every exit path.

use super::*;

#[test]
fn rejected_claim_does_not_release_an_existing_writer() {
    let core = Core::new().unwrap();
    let mut host = Host::new();
    let mut journal = MemoryJournal::default();
    let prepared = core.prepare(request(false), &host, 1000).unwrap();
    let key = prepared.event_key().to_owned();
    journal.claim(&key, &json!({})).unwrap();
    assert!(matches!(
        core.execute(
            prepared,
            &mut host,
            &mut journal,
            &FixedClock(1100),
            &NeverCancel
        ),
        Err(Error::Journal(JournalError::AlreadyClaimed))
    ));
    assert_eq!(journal.releases, 0);
    journal.append(&key, &json!({"still_owned": true})).unwrap();
    assert!(host.invoked.is_empty());
}

#[cfg(target_os = "linux")]
mod file_journal {
    use super::*;
    use aw_core::journal::FileJournal;
    use std::{fs, path::PathBuf};

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn open_writers(directory: &Directory) -> usize {
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
            .filter(|path| path.starts_with(&directory.0))
            .count()
    }

    struct PanickingHost(Host);
    impl ProviderHost for PanickingHost {
        fn descriptor(&self, id: &str) -> Option<&Value> {
            self.0.descriptor(id)
        }
        fn invoke(&mut self, _: &Value) -> Result<ProviderResult, HostError> {
            panic!("synthetic Host unwind");
        }
    }

    #[test]
    fn reused_file_journal_closes_writers_on_success_failure_cancellation_and_unwind() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/journal-lifecycle-tests")
            .join(std::process::id().to_string());
        fs::create_dir_all(&path).unwrap();
        let directory = Directory(fs::canonicalize(path).unwrap());
        let core = Core::new().unwrap();
        let mut journal = FileJournal::new(&directory.0).unwrap();
        let mut completed = Vec::new();
        for index in 0..128 {
            let mut host = Host::new();
            let mut req = request(false);
            req.plan["event_id"] = json!(format!("lifecycle-{index}"));
            let prepared = core.prepare(req, &host, 1000).unwrap();
            let key = prepared.event_key().to_owned();
            let cancelled = CancelFlag(Rc::new(Cell::new(index % 3 == 2)));
            if index % 3 == 1 {
                host.replies = vec![Reply::Transport];
            }
            let result = core.execute(
                prepared,
                &mut host,
                &mut journal,
                &FixedClock(1100),
                &cancelled,
            );
            match index % 3 {
                0 => assert_eq!(result.unwrap().record()["decision"], "proceed"),
                1 => assert!(matches!(result, Err(Error::Host(_)))),
                _ => assert_eq!(result.unwrap().record()["decision"], "cancelled"),
            }
            assert_eq!(open_writers(&directory), 0, "event {index}");
            assert!(journal.append(&key, &json!({})).is_err());
            assert!(matches!(
                journal.claim(&key, &json!({})),
                Err(JournalError::AlreadyClaimed)
            ));
            completed.push(key);
        }
        let mut host = PanickingHost(Host::new());
        let prepared = core.prepare(request(false), &host, 1000).unwrap();
        let key = prepared.event_key().to_owned();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            core.execute(
                prepared,
                &mut host,
                &mut journal,
                &FixedClock(1100),
                &NeverCancel,
            )
        }));
        assert!(unwound.is_err());
        assert_eq!(open_writers(&directory), 0);
        completed.push(key);
        drop(journal);
        let mut restarted = FileJournal::new(&directory.0).unwrap();
        for key in completed {
            assert!(matches!(
                restarted.claim(&key, &json!({})),
                Err(JournalError::AlreadyClaimed)
            ));
            assert!(!restarted.read(&key).unwrap().is_empty());
        }
    }
}
