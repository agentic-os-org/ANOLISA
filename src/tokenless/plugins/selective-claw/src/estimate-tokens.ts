function isCjkCodePoint(cp: number): boolean {
  return (
    (cp >= 0x4e00 && cp <= 0x9fff) ||
    (cp >= 0x3400 && cp <= 0x4dbf) ||
    (cp >= 0x20000 && cp <= 0x2a6df) ||
    (cp >= 0x2a700 && cp <= 0x2b73f) ||
    (cp >= 0x2b740 && cp <= 0x2b81f) ||
    (cp >= 0x2b820 && cp <= 0x2ceaf) ||
    (cp >= 0x2ceb0 && cp <= 0x2ebef) ||
    (cp >= 0x3000 && cp <= 0x303f) ||
    (cp >= 0x3040 && cp <= 0x30ff) ||
    (cp >= 0xac00 && cp <= 0xd7af) ||
    (cp >= 0xff00 && cp <= 0xffef)
  );
}

// CJK: ~1.5 tokens/char is conservative (Rust tokenizer uses 1.0); erring high avoids budget overrun.
function estimateCodePointTokens(cp: number): number {
  if (isCjkCodePoint(cp)) return 1.5;
  if (cp > 0xffff) return 2;
  return 0.25;
}

export function estimateTokens(text: string): number {
  let tokens = 0;
  for (const char of text) {
    tokens += estimateCodePointTokens(char.codePointAt(0) ?? 0);
  }
  return Math.ceil(tokens);
}

export function truncateTextToEstimatedTokens(text: string, maxTokens: number): string {
  if (maxTokens <= 0 || !text) return "";
  let tokens = 0;
  let end = 0;
  for (const char of text) {
    const nextTokens = tokens + estimateCodePointTokens(char.codePointAt(0) ?? 0);
    if (Math.ceil(nextTokens) > maxTokens) break;
    tokens = nextTokens;
    end += char.length;
  }
  return text.slice(0, end);
}
