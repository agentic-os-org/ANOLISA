use super::*;

const MAX_ENTRIES: usize = 16_384;

impl Store {
    pub fn clear_restored(&self, expected: &[Record]) -> Result<()> {
        if expected
            .iter()
            .any(|record| record.phase != Phase::Restored)
        {
            return Err(Error::new(
                "only completed restored login records may be cleared",
            ));
        }
        let Some(directory) = self.directory(false)? else {
            return if expected.is_empty() {
                Ok(())
            } else {
                Err(Error::new("login records changed before cleanup"))
            };
        };
        if self.records_at(&directory, false)? != expected {
            return Err(Error::new("login records changed before cleanup"));
        }
        for record in expected {
            let name = CString::new(format!("{}.json", record.uid)).unwrap();
            if unsafe { nix::libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
        }
        directory.sync_all().map_err(Error::io)
    }

    pub fn records(&self) -> Result<Vec<Record>> {
        let Some(directory) = self.directory(false)? else {
            return Ok(Vec::new());
        };
        self.records_at(&directory, true)
    }

    fn records_at(&self, directory: &File, accept_temporary: bool) -> Result<Vec<Record>> {
        let mut entries = nix::dir::Dir::from(directory.try_clone().map_err(Error::io)?)
            .map_err(|error| Error::io(error.into()))?;
        let mut records = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            if index >= MAX_ENTRIES {
                return Err(Error::new("administrator login record count exceeds limit"));
            }
            let entry = entry.map_err(|error| Error::io(error.into()))?;
            let bytes = entry.file_name().to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| Error::new("invalid login record filename"))?;
            if name == ".lock" || accept_temporary && valid_temporary(name) {
                let name_bytes =
                    CString::new(name).map_err(|_| Error::new("invalid login control entry"))?;
                let mut metadata = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
                if unsafe {
                    nix::libc::fstatat(
                        directory.as_raw_fd(),
                        name_bytes.as_ptr(),
                        metadata.as_mut_ptr(),
                        nix::libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    return Err(Error::io(std::io::Error::last_os_error()));
                }
                let metadata = unsafe { metadata.assume_init() };
                let mode = metadata.st_mode;
                if metadata.st_uid != self.owner
                    || metadata.st_nlink != 1
                    || mode & nix::libc::S_IFMT != nix::libc::S_IFREG
                    || mode & 0o022 != 0
                    || metadata.st_size < 0
                    || metadata.st_size as u64 > LIMIT as u64
                    || name == ".lock" && mode & 0o077 != 0
                {
                    return Err(Error::new("unsafe administrator login record entry"));
                }
                continue;
            }
            let uid = name
                .strip_suffix(".json")
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| Error::new("unknown administrator login record entry"))?;
            if name != format!("{uid}.json") {
                return Err(Error::new("noncanonical login record filename"));
            }
            let record = self
                .read_at(directory, uid)?
                .ok_or_else(|| Error::new("login record changed during enumeration"))?;
            record.validate_shape()?;
            if record.uid != uid {
                return Err(Error::new("login record UID does not match its filename"));
            }
            records.push(record);
        }
        records.sort_by_key(|record| record.uid);
        Ok(records)
    }
}

fn valid_temporary(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
        .is_some_and(|token| {
            token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}
