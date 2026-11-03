//! Identify Python peers without treating interpreter options as entrypoints.

use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

/// Python execution modes that can load a native App entrypoint.
#[derive(Debug, PartialEq)]
pub(super) enum Entrypoint<'a> {
    /// Import a module through Python's module search path.
    Module(&'a OsStr),
    /// Execute a script relative to the peer's working directory.
    Script(&'a OsStr),
}

/// Stop at Python's execution boundary, skipping interpreter-only options.
pub(super) fn entrypoint(argv: &[impl AsRef<OsStr>]) -> Option<Entrypoint<'_>> {
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        let bytes = arg.as_bytes();
        if bytes == b"--" {
            return args.next().map(|arg| Entrypoint::Script(arg.as_ref()));
        }
        if bytes == b"-" {
            return None;
        }
        if !bytes.starts_with(b"-") {
            return Some(Entrypoint::Script(arg));
        }
        if bytes == b"--check-hash-based-pycs" {
            args.next()?;
            continue;
        }
        for (index, option) in bytes.iter().enumerate().skip(1) {
            match option {
                b'c' => return None,
                b'm' => {
                    let module = if index + 1 == bytes.len() {
                        args.next()?.as_ref()
                    } else {
                        OsStr::from_bytes(&bytes[index + 1..])
                    };
                    return Some(Entrypoint::Module(module));
                }
                b'W' | b'X' => {
                    if index + 1 == bytes.len() {
                        args.next()?;
                    }
                    break;
                }
                b'b' | b'B' | b'd' | b'E' | b'i' | b'I' | b'O' | b'P' | b'q' | b'R' | b's'
                | b'S' | b'u' | b'v' | b'x' => {}
                _ => return None,
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_options_preserve_module_and_script_boundaries() {
        use Entrypoint::{Module, Script};
        for (args, expected) in [
            (vec!["-m", "qwenpaw"], Some(Module(OsStr::new("qwenpaw")))),
            (
                vec!["-uB", "-m", "qwenpaw"],
                Some(Module(OsStr::new("qwenpaw"))),
            ),
            (vec!["-umqwenpaw"], Some(Module(OsStr::new("qwenpaw")))),
            (
                vec!["-W", "-m", "-X", "utf8", "-m", "other"],
                Some(Module(OsStr::new("other"))),
            ),
            (
                vec!["-Xutf8", "-Wignore", "bin/qwenpaw"],
                Some(Script(OsStr::new("bin/qwenpaw"))),
            ),
            (
                vec!["--check-hash-based-pycs", "always", "-u", "bin/qwenpaw"],
                Some(Script(OsStr::new("bin/qwenpaw"))),
            ),
            (vec!["--", "-qwenpaw"], Some(Script(OsStr::new("-qwenpaw")))),
            (vec!["-c", "pass", "-m", "qwenpaw"], None),
            (vec!["-ucpass", "-m", "qwenpaw"], None),
            (vec!["-", "-m", "qwenpaw"], None),
            (vec!["-W"], None),
            (vec!["-X"], None),
            (vec!["-m"], None),
            (vec!["--help", "-m", "qwenpaw"], None),
        ] {
            let argv: Vec<_> = std::iter::once("python").chain(args).collect();
            assert_eq!(entrypoint(&argv), expected, "{argv:?}");
        }
        let script = OsStr::from_bytes(b"bin/\xffqwenpaw");
        assert_eq!(
            entrypoint(&[OsStr::new("python"), OsStr::new("-u"), script]),
            Some(Script(script))
        );
    }
}
