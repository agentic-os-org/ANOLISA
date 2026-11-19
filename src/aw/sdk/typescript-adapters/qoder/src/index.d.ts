import type { Admission, Availability, Task } from "@anolisa/aw-sdk";

export function qoderOptions<T extends object>(
  task: Task | Admission,
  options?: T,
): {
  options: T;
  bind<Q extends { interrupt(): Promise<unknown> }>(query: Q): Q;
  readonly failure?: Error;
};

export interface NativeQuery extends AsyncIterable<unknown> {
  interrupt(): Promise<unknown>;
  close(): Promise<void>;
}

export interface QoderBridge<Q extends NativeQuery, I> {
  readonly state: "detached" | "ready" | "unavailable" | "closed";
  readonly lastError?: Error;
  query(input: I): Q;
  close(): Promise<void>;
}

/** Preinstall public Hooks; obtain the immutable Scope at the first configured boundary. */
export function createQoderBridge<
  I extends { options?: object },
  Q extends NativeQuery,
>(options: {
  configFile: string;
  /** Trusted deployment hints bound to the exact configFile bytes. */
  descriptorFile?: string;
  query: (input: I) => Q;
  /** Defaults to native_fallback; applicable enabled required events need required mode. */
  availability?: Availability;
  target?: string;
  executable?: string;
  attachTimeoutMs?: number;
  cwd?: string;
  environment?: Record<string, string>;
}): Promise<QoderBridge<Q, I>>;
