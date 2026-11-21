//! Multibyte sudo-argument regression coverage for `exit_classify`.
//!
//! Declared only from `lib.rs` (the `wrap_tests` pattern) so the cases
//! stay out of the lib/bin test overlap ratchet
//! (`scripts/check-test-inventory.sh`): the module is compiled for the
//! `--lib` target only, while `main.rs` does not declare it.

use crate::command::first_program_token;

#[test]
fn first_program_token_survives_multibyte_sudo_arguments() {
    assert_eq!(first_program_token("sudo 中文"), "中文");
    assert_eq!(first_program_token("sudo -é"), "-é");
    assert_eq!(first_program_token("sudo '中文 文件.sh'"), "'中文");
}
