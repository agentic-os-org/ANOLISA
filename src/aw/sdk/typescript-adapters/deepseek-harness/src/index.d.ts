import type { Context } from "@deepseek-ai/cordis";

/** Preinstall during Cordis boot, then wrap each complete native Bridge request. */
export function createDeepSeekBridge(options: {
  configFile: string;
  descriptorFile?: string;
  target?: string;
  executable?: string;
  attachTimeoutMs?: number;
}): Promise<{
  plugin: { name: string; inject: ["tools"]; apply(ctx: Context): void };
  runTask<T>(sessionId: string, work: () => T | Promise<T>, options?: { signal?: AbortSignal; cwd?: string }): Promise<T>;
  close(): Promise<void>;
}>;
