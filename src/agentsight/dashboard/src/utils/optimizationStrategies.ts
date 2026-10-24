import type { MessageKey } from '../i18n';

/** One presentation for a strategy across savings charts and ATIF badges. */
export interface StrategyPresentation {
  labelKey: MessageKey;
  color: string;
  bg: string;
  pie: string;
  tooltipKey: MessageKey;
}

/** Existing strategy metadata; each view retains its own unknown-ID fallback. */
export const OPTIMIZATION_STRATEGIES: Record<string, StrategyPresentation> = {
  'compress-schema':   { labelKey: 'ts.schemaCompression', color: 'text-blue-700',   bg: 'bg-blue-100',   pie: '#3b82f6', tooltipKey: 'ts.schemaCompressionTip' },
  'compress-response': { labelKey: 'ts.responseCompression', color: 'text-violet-700', bg: 'bg-violet-100', pie: '#8b5cf6', tooltipKey: 'ts.responseCompressionTip' },
  'rewrite-command':   { labelKey: 'ts.commandRewrite', color: 'text-orange-700', bg: 'bg-orange-100', pie: '#f59e0b', tooltipKey: 'ts.commandRewriteTip' },
  'compress-toon':     { labelKey: 'ts.toonEncoding', color: 'text-teal-700',  bg: 'bg-teal-100',  pie: '#14b8a6', tooltipKey: 'ts.toonEncodingTip' },
};
