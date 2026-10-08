// Rendering of the risk conclusion the policy DSL emitted.
//
// The `because` clause is authored in English by the enforcer/policy layer, so
// the known reasons are mapped to Chinese for a Chinese UI. Every other locale
// keeps the server's own wording — translating it unconditionally printed
// Chinese inside the English Risk-cases panel, which is the default locale —
// and an unknown reason always falls back to the original text unchanged.

const ruleReasonZh: Record<string, string> = {
  'credential-derived data reached an untrusted network target': '凭据衍生数据访问了不可信网络目标',
  'credential reached an untrusted target': '凭据数据访问了不可信目标',
  'credential taint reached unknown public endpoint': '凭据污点数据到达未知公网目标',
  'agentsight sensitive file policy': 'AgentSight 敏感文件策略',
};

export function translateRuleReason(reason: string, locale: string): string {
  if (!reason || !locale.startsWith('zh')) return reason;
  const key = reason.trim().toLowerCase();
  return ruleReasonZh[key] ?? reason;
}
