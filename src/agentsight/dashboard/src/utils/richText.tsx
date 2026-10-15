// Rendering helper for the small amount of markup the analysis pipeline is
// allowed to put into finding texts. The server (agentsight-opt) documents
// `<code>` and `<b>` as the only tags these strings may carry, but the strings
// themselves are assembled from tool commands, user queries and model output,
// so anything else in them must render as literal text, never as HTML.

import React from 'react';

/** Tags the finding texts are allowed to use (opening and closing forms). */
const ALLOWED_TAGS = ['code', 'b'] as const;

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * Escape every HTML character in `text`, then restore the two documented
 * tags. Anything else — `<img onerror=…>`, `<script>`, `<code onclick=…>` —
 * stays escaped and renders as literal text.
 */
export function escapeRichText(text: string): string {
  let html = escapeHtml(text);
  for (const tag of ALLOWED_TAGS) {
    html = html
      .replace(new RegExp(`&lt;${tag}&gt;`, 'g'), `<${tag}>`)
      .replace(new RegExp(`&lt;/${tag}&gt;`, 'g'), `</${tag}>`);
  }
  return html;
}

/** Render a finding text: safe tags become real markup, everything else text. */
export const RichText: React.FC<{ children: string }> = ({ children }) => (
  <span
    className="[&_code]:bg-gray-100 [&_code]:px-1 [&_code]:py-0.5 [&_code]:rounded [&_code]:font-mono [&_code]:text-xs [&_code]:text-gray-800"
    dangerouslySetInnerHTML={{ __html: escapeRichText(children) }}
  />
);
