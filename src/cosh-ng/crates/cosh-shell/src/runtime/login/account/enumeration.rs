use super::{Account, Error, Result};

const MAX_ACCOUNTS: usize = 16_384;

pub(super) fn parse_local_accounts(passwd: &str) -> Result<Vec<Account>> {
    let mut accounts = Vec::new();
    for line in passwd
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if accounts.len() >= MAX_ACCOUNTS {
            return Err(Error::new("local account count exceeds limit"));
        }
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err(Error::new("invalid local account database"));
        }
        accounts.push(Account {
            name: fields[0].into(),
            uid: fields[2]
                .parse()
                .map_err(|_| Error::new("invalid local account UID"))?,
            gid: fields[3]
                .parse()
                .map_err(|_| Error::new("invalid local account GID"))?,
            home: fields[5].into(),
            shell: fields[6].into(),
        });
    }
    Ok(accounts)
}

pub(in crate::runtime::login) fn local_accounts() -> Result<Vec<Account>> {
    super::require_platform()?;
    let bytes = super::read_trusted(std::path::Path::new("/etc/passwd"), 1024 * 1024)?;
    let passwd =
        String::from_utf8(bytes).map_err(|_| Error::new("invalid local account encoding"))?;
    parse_local_accounts(&passwd)
}

#[cfg(target_os = "linux")]
pub(in crate::runtime::login) fn nss_accounts() -> Result<Vec<Account>> {
    super::require_platform()?;
    struct Enumeration;
    impl Drop for Enumeration {
        fn drop(&mut self) {
            unsafe {
                nix::libc::endpwent();
            }
        }
    }
    unsafe {
        nix::libc::setpwent();
    }
    let _enumeration = Enumeration;
    let mut accounts = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<nix::libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            nix::libc::getpwent_r(
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if result.is_null() && (status == 0 || status == nix::libc::ENOENT) {
            break;
        }
        if status != 0 || result.is_null() {
            return Err(Error::new(
                "NSS account enumeration failed or exceeded entry size limit",
            ));
        }
        if accounts.len() >= MAX_ACCOUNTS {
            return Err(Error::new("NSS account count exceeds limit"));
        }
        accounts.push(super::account_from_entry(&unsafe { entry.assume_init() })?);
    }
    finish_nss_accounts(accounts)
}

#[cfg(any(target_os = "linux", test))]
fn finish_nss_accounts(accounts: Vec<Account>) -> Result<Vec<Account>> {
    if accounts.is_empty() {
        return Err(Error::new("NSS account enumeration returned no identities"));
    }
    Ok(accounts)
}

#[cfg(not(target_os = "linux"))]
pub(in crate::runtime::login) fn nss_accounts() -> Result<Vec<Account>> {
    Err(Error::new(
        "NSS account enumeration is supported only on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_account_enumeration_uses_the_same_complete_identity_parser() {
        let passwd="root:x:0:0:Root:/root:/bin/bash\n# comment\nalice:x:1001:1001:A:/home/alice:/usr/bin/cosh\n";
        let users = parse_local_accounts(passwd).unwrap();
        assert_eq!(users.len(), 2);
        let expected = Account {
            name: "alice".into(),
            uid: 1001,
            gid: 1001,
            home: "/home/alice".into(),
            shell: "/usr/bin/cosh".into(),
        };
        assert_eq!(users[1], expected);
        assert_eq!(
            super::super::match_local(expected, passwd).unwrap(),
            users[1]
        );
        for bad in [
            "alice:x:1001:bad:A:/home/alice:/bin/bash\n",
            "alice:x:bad:1001:A:/home/alice:/bin/bash\n",
            "bad row\n",
        ] {
            assert!(parse_local_accounts(bad).is_err());
        }
    }
}

#[cfg(test)]
mod nss_tests {
    use super::*;
    #[test]
    fn nss_enumeration_requires_a_nonempty_complete_result() {
        assert!(finish_nss_accounts(Vec::new()).is_err());
    }
    #[test]
    fn nss_enumeration_preserves_directory_names_without_relaxing_management_names() {
        let name = std::ffi::CString::new("alice@directory.example").unwrap();
        let home = std::ffi::CString::new("/home/alice").unwrap();
        let shell = std::ffi::CString::new("/usr/bin/cosh").unwrap();
        let mut entry: nix::libc::passwd = unsafe { std::mem::zeroed() };
        entry.pw_name = name.as_ptr().cast_mut();
        entry.pw_dir = home.as_ptr().cast_mut();
        entry.pw_shell = shell.as_ptr().cast_mut();
        entry.pw_uid = 1001;
        entry.pw_gid = 1001;
        let account = super::super::account_from_entry(&entry).unwrap();
        assert_eq!(account.name, "alice@directory.example");
        assert!(super::super::lookup_nss(Some(&account.name), 1001).is_err());
        assert_eq!(finish_nss_accounts(vec![account]).unwrap().len(), 1);
    }
}
