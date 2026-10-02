use crate::hooks::linux_memory::{HighMemoryProcessHook, MemoryPressureHook};
use crate::hooks::BuiltinHook;

pub fn default_builtin_hooks() -> Vec<Box<dyn BuiltinHook>> {
    vec![
        Box::new(HighMemoryProcessHook::new()),
        Box::new(MemoryPressureHook::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_builtin_hooks_include_output_diagnostics() {
        let hooks = default_builtin_hooks();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].id(), "high-memory-process");
        assert_eq!(hooks[1].id(), "memory-pressure");
    }
}
