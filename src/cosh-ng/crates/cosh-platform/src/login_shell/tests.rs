use super::{
    storage::Table,
    system::{self, Account, Accounts},
    *,
};
use std::{
    cell::{Cell, RefCell},
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
};

struct Fixture {
    _directory: tempfile::TempDir,
    shell: PathBuf,
    fallback: PathBuf,
    table: PathBuf,
    lock: PathBuf,
    accounts: FakeAccounts,
}

#[derive(Default)]
struct FakeAccounts {
    entries: RefCell<Vec<Account>>,
    updates: Cell<usize>,
    reads: Cell<usize>,
    edit_on_second_read: bool,
    fail_update: bool,
    fail_enumeration: bool,
    local_uid: Option<u32>,
    access_error: bool,
    access_checks: Cell<usize>,
    checked_target: RefCell<Option<PathBuf>>,
}

impl Accounts for FakeAccounts {
    fn check_access(&self, account: &Account, shell: &Path) -> Result<(), Error> {
        self.access_checks.set(self.access_checks.get() + 1);
        *self.checked_target.borrow_mut() = Some(shell.into());
        if self.access_error {
            return Err(Error::AccessDenied(account.name.clone()));
        }
        Ok(())
    }
    fn all(&self) -> Result<Vec<Account>, Error> {
        if self.fail_enumeration {
            return Err(Error::Backend("fixture enumeration failed".into()));
        }
        Ok(self.entries.borrow().clone())
    }
    fn local(&self, user: &str) -> Result<Account, Error> {
        self.reads.set(self.reads.get() + 1);
        let mut account = self
            .entries
            .borrow()
            .iter()
            .find(|a| a.name == user)
            .unwrap()
            .clone();
        if let Some(uid) = self.local_uid {
            account.uid = uid;
        }
        if self.edit_on_second_read && self.reads.get() == 2 {
            account.shell = PathBuf::from("/administrator/shell");
        }
        Ok(account)
    }
    fn set_shell(&self, user: &str, shell: &Path) -> Result<(), Error> {
        if self.fail_update {
            return Err(Error::Backend("fixture usermod failed".into()));
        }
        self.updates.set(self.updates.get() + 1);
        self.entries
            .borrow_mut()
            .iter_mut()
            .find(|a| a.name == user)
            .unwrap()
            .shell = shell.into();
        Ok(())
    }
}

impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/login-shell-fixtures");
        fs::create_dir_all(&base).unwrap();
        let directory = tempfile::tempdir_in(fs::canonicalize(base).unwrap()).unwrap();
        let shell = directory.path().join("cosh");
        let fallback = directory.path().join("bash");
        for path in [&shell, &fallback] {
            fs::write(path, b"fixture executable, never executed\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let table = directory.path().join("shells");
        fs::write(
            &table,
            format!("# administrator comment\n{}\n", fallback.display()),
        )
        .unwrap();
        let lock = directory.path().join("manager.lock");
        let accounts = FakeAccounts {
            entries: RefCell::new(vec![Account {
                name: "alice".into(),
                uid: 1000,
                gid: 1000,
                shell: fallback.clone(),
            }]),
            ..Default::default()
        };
        Self {
            _directory: directory,
            shell,
            fallback,
            table,
            lock,
            accounts,
        }
    }
    fn request(&self, operation: Operation) -> Request {
        Request {
            operation,
            shell: self.shell.clone(),
            user: Some("alice".into()),
            expect_shell: None,
            restore_shell: None,
            dry_run: false,
            if_missing: false,
        }
    }
    fn run(&self, request: &Request) -> Result<LoginShellReport, Error> {
        system::run(request, &self.table, &self.lock, true, &self.accounts)
    }
}

#[test]
fn status_and_dry_run_are_unprivileged_and_write_nothing() {
    let f = Fixture::new();
    let original = fs::read(&f.table).unwrap();
    let status = system::run(
        &f.request(Operation::Status),
        &f.table,
        &f.lock,
        false,
        &f.accounts,
    )
    .unwrap();
    assert!(status.executable);
    assert!(!status.registered);
    assert_eq!(status.account_shell, Some(f.fallback.clone()));
    let mut request = f.request(Operation::Register);
    request.dry_run = true;
    assert!(
        system::run(&request, &f.table, &f.lock, false, &f.accounts)
            .unwrap()
            .changed
    );
    assert_eq!(fs::read(&f.table).unwrap(), original);
    assert!(!f.lock.exists());
    request.dry_run = false;
    assert!(matches!(
        system::run(&request, &f.table, &f.lock, false, &f.accounts),
        Err(Error::Permission)
    ));
    assert!(!f.lock.exists());
}

#[test]
fn registration_is_idempotent_and_preserves_comments_symlinks_and_metadata() {
    let f = Fixture::new();
    let target = f.table.with_file_name("managed-shells");
    fs::rename(&f.table, &target).unwrap();
    symlink("managed-shells", &f.table).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    rustix::fs::setxattr(
        &target,
        "user.fixture",
        b"keep",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let original = fs::read(&target).unwrap();
    let meta = fs::metadata(&target).unwrap();
    assert!(f.run(&f.request(Operation::Register)).unwrap().changed);
    let inode = fs::metadata(&target).unwrap().ino();
    assert!(!f.run(&f.request(Operation::Register)).unwrap().changed);
    assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
    assert!(f.run(&f.request(Operation::Unregister)).unwrap().changed);
    assert!(!f.run(&f.request(Operation::Unregister)).unwrap().changed);
    assert_eq!(
        fs::read_link(&f.table).unwrap(),
        PathBuf::from("managed-shells")
    );
    assert_eq!(fs::read(&target).unwrap(), original);
    let after = fs::metadata(&target).unwrap();
    assert_eq!(
        (after.mode(), after.uid(), after.gid()),
        (meta.mode(), meta.uid(), meta.gid())
    );
    let mut value = [0; 4];
    assert_eq!(
        rustix::fs::getxattr(&target, "user.fixture", &mut value).unwrap(),
        4
    );
    assert_eq!(&value, b"keep");
}

#[test]
fn missing_table_and_unterminated_lines_preserve_content() {
    let f = Fixture::new();
    fs::write(&f.table, b"# keep without final newline").unwrap();
    f.run(&f.request(Operation::Register)).unwrap();
    assert!(fs::read(&f.table)
        .unwrap()
        .starts_with(b"# keep without final newline\n"));
    fs::remove_file(&f.table).unwrap();
    assert!(!f.run(&f.request(Operation::Unregister)).unwrap().changed);
    assert!(!f.table.exists());
    f.run(&f.request(Operation::Register)).unwrap();
    assert!(Table::read(&f.table).unwrap().contains(&f.shell));
}

#[test]
fn interrupted_or_conflicting_preparation_never_replaces_current_table() {
    let f = Fixture::new();
    let table = Table::read(&f.table).unwrap();
    let candidate = table.changed(&f.shell, true);
    fs::write(&f.table, b"# administrator edited while update prepared\n").unwrap();
    let edited = fs::read(&f.table).unwrap();
    assert!(matches!(table.replace(&candidate), Err(Error::Conflict(_))));
    assert_eq!(fs::read(&f.table).unwrap(), edited);
    assert!(!fs::read_dir(f.table.parent().unwrap()).unwrap().any(|p| p
        .unwrap()
        .file_name()
        .as_encoded_bytes()
        .starts_with(b".cosh-shells-")));
    let guard = storage::lock(&f.lock).unwrap();
    assert!(matches!(
        f.run(&f.request(Operation::Register)),
        Err(Error::Conflict(_))
    ));
    drop(guard);
}

#[test]
fn changed_symlink_and_hardlink_are_not_overwritten() {
    let f = Fixture::new();
    let original = Table::read(&f.table).unwrap();
    let other = f.table.with_file_name("other-shells");
    fs::write(&other, b"# different administrator target\n").unwrap();
    fs::remove_file(&f.table).unwrap();
    symlink(&other, &f.table).unwrap();
    assert!(matches!(
        original.replace(b"replacement"),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        fs::read(&other).unwrap(),
        b"# different administrator target\n"
    );
    fs::hard_link(&other, f.table.with_file_name("hard-link")).unwrap();
    assert!(matches!(Table::read(&f.table), Err(Error::Conflict(_))));
}

#[test]
fn explicit_set_restore_is_idempotent_and_preserves_administrator_changes() {
    let f = Fixture::new();
    f.run(&f.request(Operation::Register)).unwrap();
    let mut set = f.request(Operation::Set);
    set.expect_shell = Some(f.fallback.clone());
    let result = f.run(&set).unwrap();
    assert_eq!(result.previous_shell, Some(f.fallback.clone()));
    assert_eq!(result.account_shell, Some(f.shell.clone()));
    assert!(!f.run(&set).unwrap().changed);
    assert!(matches!(
        f.run(&f.request(Operation::Unregister)),
        Err(Error::Conflict(_))
    ));
    let mut restore = f.request(Operation::Restore);
    restore.expect_shell = Some(f.shell.clone());
    restore.restore_shell = Some(f.fallback.clone());
    assert!(f.run(&restore).unwrap().changed);
    assert!(!f.run(&restore).unwrap().changed);
    assert_eq!(f.accounts.updates.get(), 2);
    f.accounts.entries.borrow_mut()[0].shell = "/administrator/shell".into();
    assert!(matches!(f.run(&restore), Err(Error::Conflict(_))));
    assert_eq!(f.accounts.updates.get(), 2);
}

#[test]
fn replacement_provider_and_in_use_shell_block_automatic_removal() {
    let f = Fixture::new();
    f.run(&f.request(Operation::Register)).unwrap();
    f.accounts.entries.borrow_mut()[0].shell = f.shell.clone();
    let mut request = f.request(Operation::Unregister);
    request.if_missing = true;
    let preserved = f.run(&request).unwrap();
    assert!(preserved.retained_provider && preserved.registered && !preserved.changed);
    fs::remove_file(&f.shell).unwrap();
    assert!(matches!(f.run(&request), Err(Error::Conflict(_))));
    f.accounts.entries.borrow_mut()[0].shell = f.fallback.clone();
    assert!(f.run(&request).unwrap().changed);
}

#[test]
fn dry_run_account_change_does_not_call_usermod() {
    let f = Fixture::new();
    f.run(&f.request(Operation::Register)).unwrap();
    let mut request = f.request(Operation::Set);
    request.expect_shell = Some(f.fallback.clone());
    request.dry_run = true;
    let report = system::run(&request, &f.table, &f.lock, false, &f.accounts).unwrap();
    assert!(report.changed);
    assert_eq!(report.account_shell, Some(f.fallback.clone()));
    assert_eq!(f.accounts.updates.get(), 0);
    assert!(!report.account_access_checked);
    assert_eq!(f.accounts.access_checks.get(), 0);
}

#[test]
fn account_permission_denial_prevents_set_and_restore() {
    for operation in [Operation::Set, Operation::Restore] {
        let mut f = Fixture::new();
        f.run(&f.request(Operation::Register)).unwrap();
        let mut request = f.request(operation);
        let target = if operation == Operation::Restore {
            f.accounts.entries.borrow_mut()[0].shell = f.shell.clone();
            request.expect_shell = Some(f.shell.clone());
            request.restore_shell = Some(f.fallback.clone());
            f.fallback.clone()
        } else {
            request.expect_shell = Some(f.fallback.clone());
            f.shell.clone()
        };
        // Mode-only validation passes; the credential probe must still reject.
        fs::set_permissions(&target, fs::Permissions::from_mode(0o100)).unwrap();
        f.accounts.access_error = true;
        let previous = f.accounts.entries.borrow()[0].clone();
        assert!(matches!(f.run(&request), Err(Error::AccessDenied(_))));
        assert_eq!(f.accounts.updates.get(), 0);
        assert_eq!(f.accounts.entries.borrow()[0], previous);
        assert_eq!(*f.accounts.checked_target.borrow(), Some(target));
        request.dry_run = true;
        assert!(matches!(f.run(&request), Err(Error::AccessDenied(_))));
        f.accounts.access_error = false;
        let preview = f.run(&request).unwrap();
        assert!(preview.account_access_checked && preview.changed);
        assert_eq!(f.accounts.updates.get(), 0);
    }
}

#[test]
fn repeated_separators_cannot_bypass_in_use_protection() {
    let f = Fixture::new();
    let mut request = f.request(Operation::Register);
    request.shell = PathBuf::from(format!("{}//cosh", f.shell.parent().unwrap().display()));
    f.run(&request).unwrap();
    f.accounts.entries.borrow_mut()[0].shell = f.shell.clone();
    request.operation = Operation::Status;
    assert_eq!(f.run(&request).unwrap().using_accounts, vec!["alice"]);
    assert!(f.run(&request).unwrap().registered);
    // Shell-table identity deliberately preserves the public entry spelling.
    assert!(!f.run(&f.request(Operation::Status)).unwrap().registered);
    fs::remove_file(&f.shell).unwrap();
    request.operation = Operation::Unregister;
    assert!(matches!(f.run(&request), Err(Error::Conflict(_))));
    assert!(Table::read(&f.table).unwrap().contains(&request.shell));
}

#[test]
fn account_preflight_and_tool_failures_are_visible() {
    let mut f = Fixture::new();
    f.run(&f.request(Operation::Register)).unwrap();
    let mut request = f.request(Operation::Set);
    request.expect_shell = Some(f.fallback.clone());
    f.accounts.edit_on_second_read = true;
    assert!(matches!(f.run(&request), Err(Error::Conflict(_))));
    assert_eq!(f.accounts.updates.get(), 0);
    f.accounts.edit_on_second_read = false;
    f.accounts.fail_update = true;
    assert!(matches!(f.run(&request), Err(Error::Backend(_))));
    assert_eq!(f.accounts.updates.get(), 0);
    f.accounts.fail_update = false;
    f.accounts.fail_enumeration = true;
    assert!(matches!(
        f.run(&f.request(Operation::Unregister)),
        Err(Error::Backend(_))
    ));
    assert!(Table::read(&f.table).unwrap().contains(&f.shell));
}

#[test]
fn entry_paths_keep_public_symlink_spelling_and_reject_ambiguous_input() {
    assert_eq!(
        entry_path(Path::new("/opt/anolisa/bin/cosh-cli"), None).unwrap(),
        PathBuf::from("/opt/anolisa/bin/cosh")
    );
    for path in [
        "cosh",
        "/tmp/a b",
        "/tmp/a\nb",
        "/tmp/../cosh",
        "/tmp/cosh#comment",
        "/tmp/a:b",
    ] {
        assert!(
            entry_path(Path::new("/usr/bin/cosh-cli"), Some(Path::new(path))).is_err(),
            "{path}"
        );
    }
    let f = Fixture::new();
    let mut request = f.request(Operation::Set);
    assert!(matches!(f.run(&request), Err(Error::Invalid(_))));
    request.expect_shell = Some(f.fallback.clone());
    request.user = Some("-root".into());
    assert!(matches!(f.run(&request), Err(Error::Invalid(_))));
}

#[test]
fn empty_passwd_shell_can_be_selected_with_an_explicit_empty_expectation() {
    let f = Fixture::new();
    f.accounts.entries.borrow_mut()[0].shell = PathBuf::new();
    assert_eq!(
        f.run(&f.request(Operation::Status)).unwrap().account_shell,
        Some(PathBuf::new())
    );
    f.run(&f.request(Operation::Register)).unwrap();
    let mut request = f.request(Operation::Set);
    request.expect_shell = Some(PathBuf::new());
    let changed = f.run(&request).unwrap();
    assert_eq!(changed.previous_shell, Some(PathBuf::new()));
    assert_eq!(changed.account_shell, Some(f.shell.clone()));
}

#[test]
fn account_selection_rejects_nss_local_identity_mismatch_and_duplicate_names() {
    let mut f = Fixture::new();
    f.run(&f.request(Operation::Register)).unwrap();
    let mut request = f.request(Operation::Set);
    request.expect_shell = Some(f.fallback.clone());
    f.accounts.local_uid = Some(2000);
    assert!(matches!(f.run(&request), Err(Error::Conflict(_))));
    assert_eq!(f.accounts.updates.get(), 0);
    f.accounts.local_uid = None;
    let duplicate = f.accounts.entries.borrow()[0].clone();
    f.accounts.entries.borrow_mut().push(duplicate);
    assert!(matches!(f.run(&request), Err(Error::Conflict(_))));
    assert_eq!(f.accounts.updates.get(), 0);
}

#[test]
fn registration_does_not_inherit_a_new_directory_acl() {
    let f = Fixture::new();
    let mut acl = 2u32.to_le_bytes().to_vec();
    // Linux POSIX ACL xattr: owner, named user, group, mask, other.
    for (tag, permissions, id) in [
        (1u16, 7u16, u32::MAX),
        (2, 5, 12345),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    rustix::fs::setxattr(
        f.table.parent().unwrap(),
        "system.posix_acl_default",
        &acl,
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    let before = fs::metadata(&f.table).unwrap().mode();
    f.run(&f.request(Operation::Register)).unwrap();
    let mut bytes = [0; 1024];
    assert_eq!(
        rustix::fs::getxattr(&f.table, "system.posix_acl_access", &mut bytes),
        Err(rustix::io::Errno::NODATA)
    );
    assert_eq!(fs::metadata(&f.table).unwrap().mode(), before);
}

#[test]
fn unregister_retains_the_comment_on_an_explicitly_removed_entry() {
    let f = Fixture::new();
    fs::write(
        &f.table,
        format!(
            "# leading note\n {} # administrator note\n{}\n",
            f.shell.display(),
            f.fallback.display()
        ),
    )
    .unwrap();
    assert!(f.run(&f.request(Operation::Unregister)).unwrap().changed);
    assert_eq!(
        fs::read_to_string(&f.table).unwrap(),
        format!(
            "# leading note\n# administrator note\n{}\n",
            f.fallback.display()
        )
    );
}
