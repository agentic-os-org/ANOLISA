// tests/test-harness.ts
import type { SecurityCapability } from "../src/types.js";

type RegisteredHook = {
  hookName: string;
  handler: (...args: any[]) => Promise<any>;
  priority: number;
};

type TestResult = {
  capId: string;
  hookName: string;
  result: unknown;
  error?: Error;
  durationMs: number;
};

/**
 * 创建一个 mock OpenClawPluginApi，捕获所有 api.on() 注册。
 * 不依赖真实 OpenClaw 运行时 — 纯本地执行。
 */
function createMockApi(pluginConfig?: Record<string, any>) {
  const hooks: RegisteredHook[] = [];
  const logs: string[] = [];

  const api = {
    pluginConfig: pluginConfig ?? {},
    runtime: { version: "2026.9.2" },
    logger: {
      info: (msg: string) => logs.push(`[INFO] ${msg}`),
      error: (msg: string) => logs.push(`[ERROR] ${msg}`),
      warn: (msg: string) => logs.push(`[WARN] ${msg}`),
      debug: (msg: string) => logs.push(`[DEBUG] ${msg}`),
    },
    on: (
      hookName: string,
      handler: (...args: any[]) => Promise<any>,
      opts?: { priority?: number },
    ) => {
      hooks.push({ hookName, handler, priority: opts?.priority ?? 0 });
    },
  };

  return { api: api as any, hooks, logs };
}

/**
 * 测试单个安全能力：
 * 1. 调用 register(mockApi) 捕获 hook 注册
 * 2. 用 mockEvent 触发每个已注册的 handler
 * 3. 返回结果（含耗时、错误信息）
 */
export async function testCapability(
  cap: SecurityCapability,
  mockEvents: Record<string, Record<string, unknown>>,
  pluginConfig?: Record<string, any>,
  mockCtx?: Record<string, Record<string, unknown>>,
): Promise<TestResult[]> {
  const { api, hooks, logs } = createMockApi(pluginConfig);

  // 第一步：注册
  cap.register(api);

  // 第二步：检查每个已注册的 hook 是否有对应的 mock 事件
  const missingMocks = hooks
    .filter((h) => !(h.hookName in mockEvents))
    .map((h) => h.hookName);
  if (missingMocks.length > 0) {
    throw new Error(
      `[${cap.id}] Missing mock events for registered hooks: ${missingMocks.join(", ")}. ` +
        `Add these to mockEvents so the handler logic is actually exercised.`,
    );
  }

  // 第三步：触发每个已注册的 handler
  const results: TestResult[] = [];
  for (const hook of hooks) {
    const event = mockEvents[hook.hookName];
    const ctx = mockCtx?.[hook.hookName] ?? {};
    const start = performance.now();
    try {
      const result = await hook.handler(event, ctx);
      results.push({
        capId: cap.id,
        hookName: hook.hookName,
        result,
        durationMs: performance.now() - start,
      });
    } catch (error) {
      results.push({
        capId: cap.id,
        hookName: hook.hookName,
        result: undefined,
        error: error as Error,
        durationMs: performance.now() - start,
      });
    }
  }

  // 输出捕获的日志
  if (logs.length > 0) {
    console.log(`  logs: ${logs.join(" | ")}`);
  }

  return results;
}
