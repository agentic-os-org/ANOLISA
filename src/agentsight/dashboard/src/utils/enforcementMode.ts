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
 * the evidence for what the binding does: a `block`/`kill` clause enforces, a
 * `notify` clause audits, anything else only observes. Every clause opens with
 * its verb (see the ActPlane grammar), so the verb decides the mode — which
 * operation the clause guards (`connect endpoint`, `open file`, `unlink file`,
 * `exec`) is not part of the question, and matching only a couple of literal
 * rule shapes reported a binding that blocks `unlink file` or `exec` as
 * observing.
 */
export function legacyBindingMode(policyDsl: string): EnforcementPolicyMode {
  if (/^\s*(block|kill)\s+\S+/m.test(policyDsl)) return 'enforce';
  if (/^\s*notify\s+\S+/m.test(policyDsl)) return 'audit';
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
