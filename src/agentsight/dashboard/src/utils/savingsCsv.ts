import type { SessionSavings } from './apiClient';
import { downloadBlob } from './download';

const COLUMNS = [
  'session_id', 'agent_name', 'request_count', 'total_input_tokens',
  'total_output_tokens', 'total_tokens', 'baseline_tokens', 'saved_tokens',
  'compounded_saved', 'savings_rate', 'compounded_savings_rate', 'tool_saved',
  'mcp_saved',
] as const;

function csvCell(value: string | number): string {
  if (typeof value === 'number') return String(value);
  // Identifiers can begin with spreadsheet formula prefixes.
  const text = /^[\s]*[=+\-@\t\r\n]/.test(value) ? `'${value}` : value;
  return `"${text.replace(/"/g, '""')}"`;
}

/** Export queried session metrics; rates retain the API's fractional units. */
export function serializeSavingsCsv(sessions: readonly SessionSavings[]): string {
  const rows = sessions.map(session => COLUMNS.map(key => csvCell(session[key])).join(','));
  return '\uFEFF' + [COLUMNS.join(','), ...rows].join('\r\n') + '\r\n';
}

/** Download the displayed results without fetching another snapshot. */
export function downloadSavingsCsv(sessions: readonly SessionSavings[]): void {
  const blob = new Blob([serializeSavingsCsv(sessions)], { type: 'text/csv;charset=utf-8' });
  downloadBlob(blob, 'token-savings.csv');
}
