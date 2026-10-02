// Shared policy identifier constants. The backend uses this exact string when
// creating credential-exfiltration bindings (enforcement.rs); the frontend must
// match against the same value to correctly identify protection state.
export const CREDENTIAL_POLICY_ID = 'agentsight-credential-exfiltration';
