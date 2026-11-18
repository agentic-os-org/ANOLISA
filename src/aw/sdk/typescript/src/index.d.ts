export type Availability = "required" | "native_fallback";
export interface Capabilities {
  adapter: string;
  version: string;
  entrypoint: string;
  events: Record<string, string[]>;
  failure_actions?: Record<string, string[]>;
}
export interface Hook {
  event: string;
  step: string;
  on_error: string;
  budget_ms: number;
  replace_result?: boolean;
  skill_check?: boolean;
}
export interface Routes {
  events: Record<
    string,
    {
      budget_ms: number;
      steps: Record<string, string>;
      replacement_step?: string;
    }
  >;
  hooks: Hook[];
  policy_revision?: string;
}
export interface InstanceOptions {
  endpoint: string;
  target: string;
  capabilities: Capabilities;
  availability: Availability;
  cwd?: string;
  environment?: Record<string, string>;
  config?: string;
  timeoutMs?: number;
  reconnectMs?: number;
  /** Return true only after the application has installed the supplied callback routes. */
  install: (routes: Routes) => boolean | Promise<boolean>;
}
export class AwError extends Error {
  readonly code: string;
  readonly unavailable: boolean;
}
export class Instance {
  constructor(options: InstanceOptions);
  readonly state: "detached" | "ready" | "unavailable" | "closed";
  readonly lastError?: Error;
  readonly applicationId?: string;
  /** Publish cooperative support; attach automatically only when explicitly selected. */
  listen(options?: { automatic?: boolean }): void;
  /** Register and confirm actual installed callbacks; never starts AW or an Agent. */
  connect(): Promise<boolean>;
  /** Enable one bounded reconnect attempt at a time; known policy errors stop the loop. */
  start(): void;
  stop(): Promise<void>;
  task(options?: {
    signal?: AbortSignal;
    cwd?: string;
    environment?: Record<string, string>;
  }): Promise<Task>;
  close(): Promise<void>;
}
export class Task {
  private constructor();
  readonly fallback: boolean;
  readonly routes: Routes;
  /** Requires the sdk profile; the application must adopt block before native work. */
  emit(
    event: "input.submit",
    payload: { session_id: string; turn_id: string; text: string },
    signal?: AbortSignal,
  ): Promise<SemanticDecision>;
  emit(
    event: "tool.before",
    payload: SemanticTool,
    signal?: AbortSignal,
  ): Promise<SemanticDecision>;
  emit(
    event: "tool.after",
    payload: SemanticTool & { result: unknown },
    signal?: AbortSignal,
  ): Promise<SemanticDecision>;
  invoke(
    hook: Hook,
    payload: unknown,
    signal?: AbortSignal,
  ): Promise<{
    code: number;
    stdout: Uint8Array;
    stderr: Uint8Array;
  }>;
  /** Drain an interrupted native event; does not inherit an aborted task signal. */
  closeEvent(hook: Hook, payload: unknown): Promise<void>;
  close(): Promise<void>;
}

export interface SemanticTool {
  session_id: string;
  call_id: string;
  name: string;
  input: Record<string, unknown>;
}
export interface SemanticDecision {
  decision: "continue" | "block";
  coverage: "checked" | "unavailable" | "not_configured";
}

/** Install Sources first; prepare before protected input or the first native tool. */
export class Admission {
  constructor(
    instance: Instance,
    options: {
      events: string[];
      timeoutMs?: number;
      cwd?: string;
      environment?: Record<string, string>;
    },
  );
  readonly task?: Task;
  readonly fallback: boolean;
  readonly routes:
    | Routes
    | {
        events: Record<string, never>;
        hooks: Pick<Hook, "event" | "budget_ms">[];
      };
  prepare(signal?: AbortSignal): Promise<void>;
  emit: Task["emit"];
  invoke: Task["invoke"];
  cancel(): void;
  close(): Promise<void>;
}
