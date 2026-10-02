macro_rules! hook_action_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SlashHooksActionCancelledTitle,
            SlashHooksActionCancelledBody,
            SlashHooksActionVerbEnable,
            SlashHooksActionVerbDisable,
            SlashHooksActionQuestion,
            SlashHooksActionOptionShell,
            SlashHooksActionOptionAgent,
            SlashHooksActionOptionBoth,
            SlashHooksActionAgentEnabledBody,
            SlashHooksActionAgentDisabledBody,
            SlashHooksActionAgentErrorBody,
        );
    };
}
