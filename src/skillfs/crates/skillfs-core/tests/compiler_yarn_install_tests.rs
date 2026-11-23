//! Yarn installation must not change npm dependency-addition semantics.

use skillfs_core::compiler::compile;
use skillfs_core::env::{EnvironmentProfile, OsKind};
use std::collections::{HashMap, HashSet};

fn environment() -> EnvironmentProfile {
    EnvironmentProfile {
        os: OsKind::Linux,
        available_commands: HashSet::from(["npm".into(), "yarn".into()]),
        env_vars: HashMap::new(),
    }
}

#[test]
fn yarn_keeps_npm_dependency_and_option_arguments() {
    for input in [
        "npm install lodash\n",
        "npm install @scope/pkg@1.2.3 other\n",
        "Run: npm install -D typescript\n",
        "sudo npm install --global typescript\n",
        "npm install --package-lock-only\n",
        "npm install --omit=dev\n",
        "npm install './local package'\r\n",
        "npm install \"$PACKAGE\"",
        "npm install >install.log\n",
        "npm install \\\n  lodash\n",
    ] {
        assert_eq!(compile(input, &environment()), input);
    }
}

#[test]
fn yarn_still_rewrites_bare_install_and_other_commands() {
    for (input, expected) in [
        ("npm install\r\n", "yarn install\r\n"),
        ("  Run: npm install", "  Run: yarn install"),
        (
            "npm install # dependencies\n",
            "yarn install # dependencies\n",
        ),
        ("npm install && npm test\n", "yarn install && yarn test\n"),
        ("`npm install`", "`yarn install`"),
        (
            "npm install lodash && npm run build\n",
            "npm install lodash && yarn run build\n",
        ),
    ] {
        assert_eq!(compile(input, &environment()), expected);
    }
}

#[test]
fn yarn_keeps_command_substitution_arguments() {
    for input in [
        "npm install `printf lodash`\n",
        "npm install`printf lodash`\n",
        "npm install `\nprintf lodash\n`\n",
        "`npm install \\`printf lodash\\``\n",
    ] {
        assert_eq!(compile(input, &environment()), input);
    }
}

#[test]
fn yarn_keeps_arguments_after_combined_output_redirection() {
    for input in [
        "npm install &>install.log lodash\n",
        "npm install &>>install.log lodash\n",
        "npm install&>install.log lodash\n",
    ] {
        assert_eq!(compile(input, &environment()), input);
    }
    assert_eq!(
        compile("npm install & npm test\n", &environment()),
        "yarn install & yarn test\n"
    );
}
