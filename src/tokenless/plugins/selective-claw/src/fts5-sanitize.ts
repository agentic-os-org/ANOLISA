export function sanitizeFts5Query(raw: string): string {
  const parts: string[] = [];
  const phraseRegex = /"([^"]+)"/g;
  let match: RegExpExecArray | null;
  let lastIndex = 0;

  while ((match = phraseRegex.exec(raw)) !== null) {
    const before = raw.slice(lastIndex, match.index);
    for (const t of before.split(/\s+/).filter(Boolean)) {
      parts.push(`"${t.replace(/"/g, "")}"`);
    }
    const phrase = match[1].replace(/"/g, "").trim();
    if (phrase) {
      parts.push(`"${phrase}"`);
    }
    lastIndex = match.index + match[0].length;
  }

  for (const t of raw.slice(lastIndex).split(/\s+/).filter(Boolean)) {
    parts.push(`"${t.replace(/"/g, "")}"`);
  }

  return parts.length > 0 ? parts.join(" ") : '""';
}
