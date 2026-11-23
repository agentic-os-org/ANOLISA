import type { Admission, Instance, Routes, SemanticDecision, SemanticLifecycle, Task } from "./index.js";

export class TaskScope {
  readonly instance: Instance;
  readonly timeoutMs: number;
  readonly admission?: Admission;
  readonly task?: Task;
  constructor(instance: Instance, options?: { timeoutMs?: number });
  reserve(): Promise<void>;
  release(): Promise<void>;
  open(cwd: string, environment: Record<string, string>): Promise<void>;
  check(): void;
  policy(): Promise<Routes>;
  acknowledge(): Promise<void>;
  emit<Event extends keyof SemanticLifecycle>(
    event: Event,
    payload: SemanticLifecycle[Event],
    signal?: AbortSignal,
  ): Promise<SemanticDecision>;
  closeOwned(): Promise<void>;
}
