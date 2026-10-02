use super::specs::PathMode;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeReadonlyConfig {
    pub disabled: Vec<ReadonlyRuleKey>,
    pub overrides: Vec<RuntimeReadonlySpec>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadonlyRuleKey {
    pub command: String,
    pub subcommand: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReadonlySpec {
    pub command: String,
    pub validator: RuntimeValidator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeValidator {
    Bare,
    Generic(RuntimeGenericSpec),
    Subcommand(RuntimeSubcommandSpec),
    VersionCheck(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGenericSpec {
    pub short_flags: String,
    pub long_flags: Vec<String>,
    pub value_flags: Vec<(String, Option<u32>)>,
    pub deny_flags: Vec<String>,
    pub path_mode: PathMode,
    pub bare_number_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSubcommandSpec {
    pub deny_args: Vec<String>,
    pub subcommands: Vec<(String, RuntimeValidator)>,
}

impl ReadonlyRuleKey {
    pub fn command(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            subcommand: None,
        }
    }

    pub fn subcommand(command: impl Into<String>, subcommand: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            subcommand: Some(subcommand.into()),
        }
    }

    pub(super) fn matches(&self, command: &str, subcommand: Option<&str>) -> bool {
        if self.command != command {
            return false;
        }
        match self.subcommand.as_deref() {
            Some(expected) => Some(expected) == subcommand,
            None => true,
        }
    }
}
