macro_rules! legacy_approval_mode_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalModeRemovedBody,
            ApprovalModeRemovedFooter,
        );
    };
}

macro_rules! mode_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ModeTitle,
            ModesTitle,
            ModeApprovalLine,
            ModeAnalysisLine,
            ModeSummaryFooter,
            ModeRemovedTitle,
            ModeRemovedBody,
            ModeRemovedFooter,
            ModeLanguageBody,
            ModeLanguageFooter,
            ModeUnknownBody,
            ModeUnknownFooter,
            ApprovalModeTitle,
            ApprovalModeSetBody,
            ApprovalModeUnknownBody,
            ApprovalModeUsageFooter,
            ApprovalModeRecommendFooter,
            ApprovalModeAutoFooter,
            ApprovalModeTrustFooter,
            ApprovalModeTrustConfirmationTitle,
            ApprovalModeTrustConfirmationBody,
            ApprovalModeTrustConfirmationCommandBody,
            ApprovalModeTrustConfirmationFooter,
            ApprovalModeCardTitle,
            ApprovalModeCardCurrentLine,
            ApprovalModeCardRecommendLine,
            ApprovalModeCardAutoLine,
            ApprovalModeCardTrustLine,
            ApprovalModeCardFooter,
            ApprovalModeRemainsBody,
            ApprovalModeCancelBody,
            ApprovalModeCancelFooter,
            AnalysisModeTitle,
            AnalysisModeCurrentBody,
            AnalysisModeSetBody,
            AnalysisModeUnknownBody,
            AnalysisModeUsageFooter,
            AnalysisModeSmartFooter,
            AnalysisModeAutoFooter,
            AnalysisModeManualFooter,
            AnalysisModeCardSmartLine,
            AnalysisModeCardAutoLine,
            AnalysisModeCardManualLine,
            AnalysisModeCardFooter,
            AnalysisModeRemainsBody,
            AnalysisModeCancelBody,
            AnalysisModeCancelFooter,
        );
    };
}

// Appended as a trailing segment so existing MessageId discriminants stay stable.
macro_rules! enhanced_routing_mode_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpSummaryModeRouting,
            ModeRoutingLine,
            RoutingModeTitle,
            RoutingModeCurrentBody,
            RoutingModeSetBody,
            RoutingModeUnavailableBody,
            RoutingModeUnknownBody,
            RoutingModeUsageFooter,
            RoutingModeAssistedFooter,
            RoutingModeShellOnlyFooter,
        );
    };
}
