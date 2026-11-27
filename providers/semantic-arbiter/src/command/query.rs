//! Narrow query forms for programs whose other arguments can name writes.

use super::Word;

pub(super) fn read_only(name: &str, args: &[Word]) -> bool {
    if matches!(name, "echo" | "printf") {
        return true;
    }
    if args.iter().any(|word| word.dynamic) {
        return false;
    }
    let texts: Vec<_> = args.iter().map(|word| word.text.as_str()).collect();
    match name {
        "date" => date_query(&texts),
        "hostname" => texts.iter().all(|arg| {
            matches!(
                *arg,
                "-s" | "--short"
                    | "-f"
                    | "--fqdn"
                    | "-d"
                    | "--domain"
                    | "-i"
                    | "--ip-address"
                    | "-I"
                    | "--all-ip-addresses"
                    | "--help"
                    | "--version"
            )
        }),
        "xxd" => xxd_query(&texts),
        // Filter programs and pagers have execution/output modes outside this projection.
        "jq" | "less" | "more" => false,
        "uniq" => texts.iter().filter(|arg| !arg.starts_with('-')).count() <= 1,
        "tree" => !texts
            .iter()
            .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains('o')),
        "file" => !texts.iter().any(|arg| {
            *arg == "--compile"
                || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('C'))
        }),
        "rg" => !texts
            .iter()
            .any(|arg| *arg == "--pre" || arg.starts_with("--pre=")),
        "diff" => !texts
            .iter()
            .any(|arg| *arg == "--output" || arg.starts_with("--output=")),
        _ => true,
    }
}

fn date_query(args: &[&str]) -> bool {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        match *arg {
            "-u" | "--utc" | "--universal" | "-R" | "--rfc-email" | "-I" | "--iso-8601"
            | "--help" | "--version" => {}
            "-d" | "--date" | "-r" | "--reference" => {
                if args.get(index).is_none() {
                    return false;
                }
                index += 1;
            }
            text if text.starts_with('+')
                || text.starts_with("--date=")
                || text.starts_with("--reference=")
                || text.starts_with("--iso-8601=")
                || text.starts_with("--rfc-3339=") => {}
            _ => return false,
        }
    }
    true
}

fn xxd_query(args: &[&str]) -> bool {
    let mut index = 0;
    let mut inputs = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        match *arg {
            "-a" | "-b" | "-e" | "-p" | "-ps" | "-u" | "-i" | "-v" => {}
            "-l" | "-s" | "-c" | "-g" => {
                if args.get(index).is_none() {
                    return false;
                }
                index += 1;
            }
            text if !text.starts_with('-') || text == "-" => inputs += 1,
            _ => return false,
        }
    }
    inputs <= 1
}
