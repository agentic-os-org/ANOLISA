export type RuntimeLlmCompleteFn = (params: {
  messages: Array<{ role: "system" | "user" | "assistant"; content: string }>;
  systemPrompt?: string;
  maxTokens?: number;
  purpose?: string;
}) => Promise<{ text: string }>;

export type SummarizeFn = (text: string) => Promise<string>;

const SYSTEM_PROMPT = `Summarize this conversation turn. Your summary must:
1. Preserve key entities: function names, class names, file paths, API endpoints, config keys
2. State the core question/request and the conclusion/answer reached
3. Note any decisions made or code changes proposed

Output a concise paragraph (2-4 sentences). No preamble, no bullet points.`;

export function createSummarizer(
  runtimeLlmComplete: RuntimeLlmCompleteFn,
): SummarizeFn {
  return async (text: string): Promise<string> => {
    try {
      const result = await runtimeLlmComplete({
        messages: [{ role: "user", content: text }],
        systemPrompt: SYSTEM_PROMPT,
        purpose: "selective-claw turn summary",
      });
      const rawText = result?.text?.trim() ?? "";
      const summary = stripThinkingTags(rawText);
      if (summary.length > 0) {
        return summary;
      }
      console.warn("[selective-claw] LLM returned empty text, using fallback");
      return fallbackSummary(text);
    } catch (err) {
      console.error("[selective-claw] summarize failed:", err);
      return fallbackSummary(text);
    }
  };
}

function stripThinkingTags(text: string): string {
  return text.replace(/<think>[\s\S]*?<\/think>/g, "").trim();
}

export function fallbackSummary(text: string): string {
  const firstLine = text.split("\n").find((line) => line.trim().length > 0) ?? text;
  const clean = firstLine.replace(/^(user|assistant|tool):\s*/i, "").trim();
  if (clean.length <= 80) return clean;
  return clean.slice(0, 80) + "...";
}
