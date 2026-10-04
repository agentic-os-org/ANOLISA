import type { EnforcementPolicyMode } from './apiClient';

/** The fields of a binding request this module needs. */
export interface BindingModeSource {
  policy_mode?: EnforcementPolicyMode | null;
  policy_dsl: string;
}

/**
 * Mode of a binding stored before `policy_mode` existed.
 *
 * Enforcers predating that field persisted only the policy DSL, which is then
 * the evidence for what the binding does: `block …` rules enforce, `notify …`
 * rules audit, anything else only observes.
 */
export function legacyBindingMode(policyDsl: string): EnforcementPolicyMode {
  if (/\bblock connect endpoint\b/.test(policyDsl) || /\bblock open file\b/.test(policyDsl)) {
    return 'enforce';
  }
  if (/\bnotify connect endpoint\b/.test(policyDsl)) {
    return 'audit';
  }
  return 'observe';
}

/**
 * Effective mode of a binding request, falling back to the DSL for rows that
 * predate `policy_mode`. Every view that reports how an agent is protected
 * must use this: reading `policy_mode` alone reports such a binding as
 * unprotected (or audit-only) while it is actually blocking.
 */
export function effectiveBindingMode(request: BindingModeSource): EnforcementPolicyMode {
  return request.policy_mode ?? legacyBindingMode(request.policy_dsl);
}
