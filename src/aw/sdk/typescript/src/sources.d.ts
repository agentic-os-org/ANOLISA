import type {
  Admission,
  Availability,
  Capabilities,
  Instance,
  Routes,
  SemanticDecision,
  Task,
} from "./index.js";
import { AwError } from "./index.js";

export type SourceEvent =
  | "session.start"
  | "input.submit"
  | "tool.before"
  | "tool.after"
  | "turn.stop"
  | "session.end";
export const SOURCE_PROTOCOL: "aw-hook-source/v1";
export const DEFAULT_SOURCE_EVENTS: readonly SourceEvent[];
export const DEFAULT_MAX_STREAM_ITEMS: number;
export const DEFAULT_MAX_STREAM_BYTES: number;

export class SourceDenied extends AwError {
  readonly event: SourceEvent;
  readonly sessionId?: string;
  readonly callId?: string;
  readonly turnId?: string;
  readonly toolName?: string;
  constructor(
    event: SourceEvent,
    details?: {
      sessionId?: string;
      callId?: string;
      turnId?: string;
      toolName?: string;
    },
  );
}

export class SourceResultWithheld extends AwError {
  readonly event: SourceEvent;
  readonly sessionId?: string;
  readonly callId?: string;
  readonly toolName?: string;
  readonly toolExecuted: true;
  constructor(
    event?: SourceEvent,
    details?: {
      sessionId?: string;
      callId?: string;
      toolName?: string;
    },
  );
}

export interface SourceCapabilityOptions {
  replaceInput?: boolean;
  replaceResult?: boolean;
  withholdResult?: boolean;
}

export function sourceCapabilities(
  events?: readonly SourceEvent[],
  options?: SourceCapabilityOptions,
): Capabilities;
export function confirmSourceRoutes(
  capabilities: Capabilities,
  routes: Routes,
): boolean;

export interface WrapInputOptions<Args extends unknown[] = unknown[]> {
  /** Detach an argument with application-specific state for the checked call. */
  snapshotArg?: (value: Args[number]) => Args[number];
  extractText?: (...args: Args) => string;
  extractTurnId?: (...args: Args) => string | undefined;
  extractSignal?: (...args: Args) => AbortSignal | undefined;
  stream?: boolean;
}

export interface WrapToolOptions<
  Args extends unknown[] = unknown[],
  Result = unknown,
  SnapshotItem = Result,
> {
  name?: string;
  /** Detach an argument with application-specific state for the checked call. */
  snapshotArg?: (value: Args[number]) => Args[number];
  /** Detach a non-plain tool result (or stream chunk) for strict-after audit and delivery. */
  snapshotResult?: (value: SnapshotItem) => SnapshotItem;
  extractInput?: (...args: Args) => Record<string, unknown>;
  applyInput?: (
    updatedInput: Record<string, unknown>,
    originalArgs: Args,
  ) => Args;
  extractCallId?: (...args: Args) => string | undefined;
  extractResult?: (result: Result) => unknown;
  applyResult?: (originalResult: Result, replacedResult: unknown) => Result;
  extractSignal?: (...args: Args) => AbortSignal | undefined;
  stream?: boolean;
  maxStreamItems?: number;
  maxStreamBytes?: number;
  bufferStream?: boolean;
}

export type StreamResult<T> = T extends AsyncIterable<infer Y> ? Y : never;

// TypeScript gives a generator declaration and a synchronous generator factory
// the same call signature. Only an explicit stream option fixes the outer shape.
type ImplicitResult<R> =
  | Promise<Awaited<R>>
  | (R extends AsyncIterable<infer Y>
      ? AsyncGenerator<Y, void, unknown>
      : never);

type ImplicitToolResult<R> =
  | Awaited<R>
  | (R extends AsyncIterable<infer Y> ? Y[] : never);

type EagerIterable =
  | string
  | String
  | readonly unknown[]
  | ArrayBufferView
  | ReadonlyMap<unknown, unknown>
  | ReadonlySet<unknown>;

type SnapshotItemOf<T> = T extends EagerIterable
  ? T
  : T extends AsyncIterable<infer Y>
    ? Y
    : T extends { next(): IteratorResult<infer Y, unknown> }
      ? Y
      : T extends Iterable<infer Y>
        ? Y
        : T;

type ImplicitSnapshotItem<R> = SnapshotItemOf<Awaited<R>>;

type SyncLazyItem<T> = 0 extends 1 & T
  ? never
  : T extends EagerIterable
    ? never
    : T extends AsyncIterable<unknown>
      ? never
      : T extends { next(): IteratorResult<infer Y, unknown> }
        ? Y
        : T extends Iterable<infer Y>
          ? Y
          : never;

type HasSyncLazyResult<T> = [SyncLazyItem<T>] extends [never] ? false : true;

type BufferedToolResultOf<T> = 0 extends 1 & T
  ? T
  : T extends EagerIterable
    ? T
    : T extends AsyncIterable<unknown>
      ? T
      : T extends { next(): IteratorResult<infer Y, unknown> }
        ? Y[]
        : T extends Iterable<infer Y>
          ? Y[]
          : T;

type SyncBufferedToolResult<R> =
  | BufferedToolResultOf<Awaited<R>>
  | (R extends AsyncIterable<infer Y> ? Y[] : never);

export type MethodKeys<T extends object> = {
  [K in keyof T]: T[K] extends (...args: never[]) => unknown ? K : never;
}[keyof T] &
  string;

export type InputMethodMap<T extends object> = {
  [K in MethodKeys<T>]?: T[K] extends (...args: infer A) => unknown
    ? WrapInputOptions<A>
    : never;
};

type ToolMethodName = string & {
  readonly stream?: never;
  readonly bufferStream?: never;
};

type BaseWrapToolOptions<
  Args extends unknown[],
  Result,
  SnapshotItem,
> = Omit<WrapToolOptions<Args, Result, SnapshotItem>, "stream" | "bufferStream">;

type DefaultToolMethodMap<T extends object> = {
  [K in MethodKeys<T>]?: T[K] extends (...args: infer A) => infer R
    ?
        | ToolMethodName
        | (BaseWrapToolOptions<
            A,
            ImplicitToolResult<R>,
            ImplicitSnapshotItem<R>
          > & {
            stream?: false;
            bufferStream?: false;
          })
    : never;
};

type BufferedToolMethodMap<T extends object> = {
  [K in MethodKeys<T>]?: T[K] extends (...args: infer A) => infer R
    ?
        | ToolMethodName
        | (HasSyncLazyResult<Awaited<R>> extends true
            ? BaseWrapToolOptions<
                A,
                SyncBufferedToolResult<R>,
                ImplicitSnapshotItem<R>
              > & {
                stream?: false;
                bufferStream: true;
              }
            : BaseWrapToolOptions<
                A,
                ImplicitToolResult<R>,
                ImplicitSnapshotItem<R>
              > & {
                stream?: false;
                bufferStream?: boolean;
              })
        | (Awaited<R> extends AsyncIterable<infer Y>
            ? BaseWrapToolOptions<A, Y[], Y> & {
                stream: true;
                bufferStream?: boolean;
              }
            : never)
    : never;
};

type DynamicToolMethodMap<T extends object> = {
  [K in MethodKeys<T>]?: T[K] extends (...args: infer A) => infer R
    ?
        | ToolMethodName
        | (HasSyncLazyResult<Awaited<R>> extends true
            ? BaseWrapToolOptions<
                A,
                ImplicitToolResult<R> | SyncBufferedToolResult<R>,
                ImplicitSnapshotItem<R>
              > & {
                stream?: false;
                bufferStream: boolean;
              }
            : BaseWrapToolOptions<
                A,
                ImplicitToolResult<R>,
                ImplicitSnapshotItem<R>
              > & {
                stream?: false;
                bufferStream?: boolean;
              })
        | (Awaited<R> extends AsyncIterable<infer Y>
            ? BaseWrapToolOptions<A, Y[], Y> & {
                stream: true;
                bufferStream?: boolean;
              }
            : never)
    : never;
};

type ToolMethodOptions<A extends unknown[], R> =
  | ToolMethodName
  | (HasSyncLazyResult<Awaited<R>> extends true
      ?
          | (BaseWrapToolOptions<
              A,
              ImplicitToolResult<R>,
              ImplicitSnapshotItem<R>
            > & {
              stream?: false;
              bufferStream?: false;
            })
          | (BaseWrapToolOptions<
              A,
              SyncBufferedToolResult<R>,
              ImplicitSnapshotItem<R>
            > & {
              stream?: false;
              bufferStream: true;
            })
      : BaseWrapToolOptions<
          A,
          ImplicitToolResult<R>,
          ImplicitSnapshotItem<R>
        > & {
          stream?: false;
          bufferStream?: boolean;
        })
  | (Awaited<R> extends AsyncIterable<infer Y>
      ? BaseWrapToolOptions<A, Y[], Y> & {
          stream: true;
          bufferStream?: boolean;
        }
      : never);

type PickedToolMethodMap<T extends object, K extends MethodKeys<T>> = {
  [P in K]: T[P] extends (...args: infer A) => infer R
    ? ToolMethodOptions<A, R>
    : never;
};

export type ToolMethodMap<
  T extends object,
  K extends MethodKeys<T> = never,
> = [K] extends [never]
  ? {
      [P in MethodKeys<T>]?: T[P] extends (...args: infer A) => infer R
        ? ToolMethodOptions<A, R>
        : never;
    }
  : PickedToolMethodMap<T, K>;

export interface Instrumentation {
  readonly restored: boolean;
  restore(): void;
}

export interface SourceSessionOptions {
  sessionId?: string;
  events?: readonly SourceEvent[];
  timeoutMs?: number;
  cwd?: string;
  environment?: Record<string, string>;
}

export class SourceSession {
  readonly sessionId: string;
  readonly events: readonly SourceEvent[];
  readonly admission: Admission;
  readonly task?: Task;
  readonly fallback: boolean;
  readonly closed: boolean;
  /** Aborts on session close. Pass to cancellable native stream reads. */
  readonly signal: AbortSignal;
  prepare(signal?: AbortSignal): Promise<void>;
  sessionStart(options?: { signal?: AbortSignal }): Promise<SemanticDecision>;
  /** Return the stop decision for adoption at the app's native boundary. */
  turnStop(
    turnId: string,
    reason: string,
    options?: { attemptId?: string; signal?: AbortSignal },
  ): Promise<SemanticDecision>;
  /** Report actual native termination before releasing the Scope. */
  sessionEnd(reason: string, options?: { signal?: AbortSignal }): Promise<SemanticDecision>;
  submit(
    text: string,
    options?: { turnId?: string; signal?: AbortSignal },
  ): Promise<SemanticDecision>;
  beforeTool(
    name: string,
    input: Record<string, unknown>,
    options?: { callId?: string; signal?: AbortSignal },
  ): Promise<{
    callId: string;
    input: Record<string, unknown>;
    decision: SemanticDecision;
  }>;
  afterTool(
    name: string,
    input: Record<string, unknown>,
    result: unknown,
    options: { callId: string; signal?: AbortSignal },
  ): Promise<SemanticDecision>;
  wrapInputStream<Args extends unknown[], Yield>(
    fn: (...args: Args) => AsyncIterable<Yield> | Promise<AsyncIterable<Yield>>,
    options?: Omit<WrapInputOptions<Args>, "stream">,
  ): (...args: Args) => AsyncGenerator<Yield, void, unknown>;
  wrapInput<Args extends unknown[], Yield>(
    fn: (...args: Args) => AsyncIterable<Yield> | Promise<AsyncIterable<Yield>>,
    options: WrapInputOptions<Args> & { stream: true },
  ): (...args: Args) => AsyncGenerator<Yield, void, unknown>;
  wrapInput<Args extends unknown[], Result>(
    fn: (...args: Args) => Result,
    options?: WrapInputOptions<Args> & { stream?: false },
  ): (...args: Args) => ImplicitResult<Result>;
  wrapToolStream<Args extends unknown[], Yield>(
    fn: (...args: Args) => AsyncIterable<Yield> | Promise<AsyncIterable<Yield>>,
    options?: Omit<WrapToolOptions<Args, Yield[], Yield>, "stream">,
  ): (...args: Args) => AsyncGenerator<Yield, void, unknown>;
  wrapTool<Args extends unknown[], Yield>(
    fn: (...args: Args) => AsyncIterable<Yield> | Promise<AsyncIterable<Yield>>,
    options: WrapToolOptions<Args, Yield[], Yield> & { stream: true },
  ): (...args: Args) => AsyncGenerator<Yield, void, unknown>;
  wrapTool<Args extends unknown[], Result>(
    fn: (...args: Args) => Result,
    options: WrapToolOptions<
      Args,
      SyncBufferedToolResult<Result>,
      ImplicitSnapshotItem<Result>
    > & {
      stream?: false;
      bufferStream: true;
    },
  ): (...args: Args) => ImplicitResult<Result>;
  wrapTool<Args extends unknown[], Result>(
    fn: (...args: Args) => Result,
    options?: WrapToolOptions<
      Args,
      ImplicitToolResult<Result>,
      ImplicitSnapshotItem<Result>
    > & {
      stream?: false;
      bufferStream?: false;
    },
  ): (...args: Args) => ImplicitResult<Result>;
  wrapTool<Args extends unknown[], Result>(
    fn: (...args: Args) => Result,
    options: WrapToolOptions<
      Args,
      ImplicitToolResult<Result> | SyncBufferedToolResult<Result>,
      ImplicitSnapshotItem<Result>
    > & {
      stream?: false;
      bufferStream?: boolean;
    },
  ): (...args: Args) => ImplicitResult<Result>;
  instrument<T extends object>(
    target: T,
    options?: {
      inputMethods?: readonly MethodKeys<T>[] | InputMethodMap<T>;
      toolMethods?: readonly MethodKeys<T>[] | DefaultToolMethodMap<T>;
    },
  ): Instrumentation;
  instrument<T extends object>(
    target: T,
    options: {
      inputMethods?: readonly MethodKeys<T>[] | InputMethodMap<T>;
      toolMethods: BufferedToolMethodMap<T>;
    },
  ): Instrumentation;
  instrument<T extends object>(
    target: T,
    options: {
      inputMethods?: readonly MethodKeys<T>[] | InputMethodMap<T>;
      toolMethods: DynamicToolMethodMap<T>;
    },
  ): Instrumentation;
  instrument<T extends object, K extends MethodKeys<T>>(
    target: T,
    options: {
      inputMethods?: readonly MethodKeys<T>[] | InputMethodMap<T>;
      toolMethods: PickedToolMethodMap<T, K>;
    },
  ): Instrumentation;
  instrument<T extends object>(
    target: T,
    options?: {
      inputMethods?: readonly MethodKeys<T>[] | InputMethodMap<T>;
      toolMethods?: readonly MethodKeys<T>[] | ToolMethodMap<T>;
    },
  ): Instrumentation;
  close(options?: { timeoutMs?: number }): Promise<void>;
}

export interface LanguageSourceCreateOptions extends SourceCapabilityOptions {
  endpoint: string;
  target: string;
  availability?: Availability;
  events?: readonly SourceEvent[];
  cwd?: string;
  environment?: Record<string, string>;
  config?: string;
  timeoutMs?: number;
  reconnectMs?: number;
}

export class LanguageSource {
  readonly instance: Instance;
  constructor(instance: Instance, options?: { ownsInstance?: boolean });
  static create(options: LanguageSourceCreateOptions): LanguageSource;
  connect(): Promise<boolean>;
  start(): void;
  listen(options?: { automatic?: boolean }): void;
  stop(): Promise<void>;
  session(options?: SourceSessionOptions): SourceSession;
  withSession<T>(
    options: SourceSessionOptions | undefined,
    callback: (session: SourceSession) => Promise<T>,
  ): Promise<T>;
  close(options?: {
    closeInstance?: boolean;
    timeoutMs?: number;
  }): Promise<void>;
}
