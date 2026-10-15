use std::fs;
use std::process::Command;

pub(crate) struct AuditSystemConfig {
    directory: tempfile::TempDir,
}

impl AuditSystemConfig {
    pub(crate) fn new() -> Result<Self, String> {
        if !cfg!(all(target_os = "linux", target_env = "gnu")) {
            return Err("audit system-config fixture requires Linux/glibc and cc".to_string());
        }
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source = directory.path().join("audit-config.c");
        fs::write(&source, SHIM).map_err(|error| error.to_string())?;
        let output = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-Wall", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(directory.path().join("audit-config.so"))
            .arg("-ldl")
            .output()
            .map_err(|error| format!("cannot compile audit fixture with cc: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "audit fixture compilation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let fixture = Self { directory };
        fixture.write("")?;
        Ok(fixture)
    }

    pub(crate) fn write(&self, contents: &str) -> Result<(), String> {
        fs::write(self.directory.path().join("config.toml"), contents)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn path(&self, name: &str) -> std::path::PathBuf {
        self.directory.path().join(name)
    }

    pub(crate) fn assert_read(&self) {
        assert!(
            self.path("read.marker").exists(),
            "formal Shell binary did not read the isolated system-config fixture"
        );
    }

    pub(crate) fn reset_read_marker(&self) {
        match fs::remove_file(self.path("read.marker")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot reset audit config read marker: {error}"),
        }
    }
}

const SHIM: &str = r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *redirect(const char *path) {
    if (strcmp(path, "/etc/copilot-shell/config.toml") != 0) return path;
    const char *fixture = getenv("COSH_TEST_AUDIT_SYSTEM_CONFIG");
    if (!fixture) return path;
    const char *marker = getenv("COSH_TEST_AUDIT_SYSTEM_READ_MARKER");
    if (marker) {
        int fd = syscall(SYS_openat, AT_FDCWD, marker,
                         O_WRONLY | O_CREAT | O_APPEND | O_NOFOLLOW, 0600);
        if (fd >= 0) {
            syscall(SYS_write, fd, "read\n", 5);
            syscall(SYS_close, fd);
        }
    }
    return fixture;
}

static mode_t mode_argument(int flags, va_list args) {
    return (flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE
        ? va_arg(args, mode_t) : 0;
}

#define OPEN(name) \
int name(const char *path, int flags, ...) { \
    va_list args; va_start(args, flags); \
    mode_t mode = mode_argument(flags, args); va_end(args); \
    int (*next)(const char *, int, ...) = dlsym(RTLD_NEXT, #name); \
    if (!next) { errno = ENOSYS; return -1; } \
    return next(redirect(path), flags, mode); \
}

#define OPENAT(name) \
int name(int dirfd, const char *path, int flags, ...) { \
    va_list args; va_start(args, flags); \
    mode_t mode = mode_argument(flags, args); va_end(args); \
    int (*next)(int, const char *, int, ...) = dlsym(RTLD_NEXT, #name); \
    if (!next) { errno = ENOSYS; return -1; } \
    return next(dirfd, redirect(path), flags, mode); \
}

OPEN(open)
OPEN(open64)
OPENAT(openat)
OPENAT(openat64)
"#;
