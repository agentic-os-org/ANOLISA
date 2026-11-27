mod common;

use std::ffi::{CString, OsStr};
use std::fs;
use std::io::ErrorKind::{InvalidInput, NotFound};
use std::os::unix::{ffi::OsStrExt, fs::symlink};

use common::{MountFixture, create_skill_dir};

#[test]
fn invalid_child_names_never_alias_the_replacement_character() {
    skip_if_no_fuse!();
    let raw = OsStr::from_bytes(b"note-\xff");
    let valid = OsStr::new("note-\u{fffd}");
    let fx = MountFixture::normal(|source| {
        create_skill_dir(source, "demo");
        let dir = source.join("demo");
        fs::write(dir.join(raw), b"raw-bytes").unwrap();
        fs::write(dir.join(valid), b"valid-utf8").unwrap();
    });

    let backing = fx.source_skill_path("demo");
    let dir = fx.skill_path("demo");
    let invalid_path = dir.join(raw);
    let valid_path = dir.join(valid);
    let entries: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries.iter().filter(|n| n.as_os_str() == valid).count(), 1);
    assert!(entries.iter().all(|n| n != raw));

    assert_eq!(fs::metadata(&invalid_path).unwrap_err().kind(), NotFound);
    assert_eq!(fs::read(&invalid_path).unwrap_err().kind(), NotFound);
    assert_eq!(fs::remove_file(&invalid_path).unwrap_err().kind(), NotFound);
    assert_eq!(
        fs::rename(&invalid_path, dir.join("other"))
            .unwrap_err()
            .kind(),
        NotFound
    );
    assert_eq!(fs::remove_dir(&invalid_path).unwrap_err().kind(), NotFound);
    assert_eq!(fs::read(backing.join(raw)).unwrap(), b"raw-bytes");
    assert_eq!(fs::read(backing.join(valid)).unwrap(), b"valid-utf8");

    let new_name = OsStr::from_bytes(b"new-\xff");
    let dest = dir.join(new_name);
    assert_eq!(
        fs::File::create(&invalid_path).unwrap_err().kind(),
        InvalidInput
    );
    assert_eq!(fs::create_dir(&dest).unwrap_err().kind(), InvalidInput);
    assert_eq!(
        fs::rename(&valid_path, &dest).unwrap_err().kind(),
        InvalidInput
    );
    assert_eq!(
        fs::hard_link(&valid_path, &dest).unwrap_err().kind(),
        InvalidInput
    );
    assert_eq!(symlink(valid, &dest).unwrap_err().kind(), InvalidInput);
    let fifo = CString::new(dest.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a live NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, -1);
    assert_eq!(std::io::Error::last_os_error().kind(), InvalidInput);
    assert_eq!(
        fs::symlink_metadata(backing.join(new_name))
            .unwrap_err()
            .kind(),
        NotFound
    );
    assert_eq!(
        fs::symlink_metadata(backing.join("new-\u{fffd}"))
            .unwrap_err()
            .kind(),
        NotFound
    );

    // A link target stores raw bytes. Following it goes through lookup again.
    let link = dir.join("raw-target");
    symlink(raw, &link).unwrap();
    assert_eq!(fs::read_link(&link).unwrap().as_os_str(), raw);
    assert_eq!(fs::read(&link).unwrap_err().kind(), NotFound);

    assert_eq!(fs::read(&valid_path).unwrap(), b"valid-utf8");
    fs::remove_file(valid_path).unwrap();
    assert_eq!(
        fs::metadata(backing.join(valid)).unwrap_err().kind(),
        NotFound
    );
    assert_eq!(fs::read(backing.join(raw)).unwrap(), b"raw-bytes");
}
