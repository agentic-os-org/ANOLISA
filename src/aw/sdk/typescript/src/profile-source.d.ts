import type { Availability } from "./index.js";

export type ProfileCallShape = "sync" | "async" | "sync_iter" | "async_iter";
export type ProfileCodec = "text" | "json" | "json_string";
export type ProfileFieldBinding =
  | { from: string; codec: ProfileCodec }
  | { const: string };
export type ProfileHook = {
  mode: "observe" | "control";
  effects: readonly (
    | "observe"
    | "block"
    | "replace_input"
    | "replace_result"
    | "ask"
  )[];
  failure_actions: readonly ("report" | "block" | "withhold_result")[];
};
export interface ProfileTarget {
  id: string;
  module: string;
  qualname: string;
  call: ProfileCallShape;
  hooks: Readonly<Record<string, ProfileHook>>;
  fields: Readonly<Record<string, ProfileFieldBinding>>;
  code_sha256?: string;
  stream?: { buffer: boolean; max_items: number; max_bytes: number };
}
export interface InstrumentationProfile {
  version: "aw-instrumentation-profile/v1alpha1";
  id: string;
  origin: "builtin" | "custom";
  runtime: "js";
  package: { name: string; versions: string };
  targets: readonly ProfileTarget[];
}
export interface ProfileInvocation {
  receiver: unknown;
  args: readonly unknown[];
  target: Readonly<ProfileTarget>;
}
export interface ProfileProbeOptions {
  /** Resolution starts at this directory, entry file, or file URL. */
  resolveFrom?: string | URL;
  availability?: Availability;
  resolveSessionId?: (context: ProfileInvocation) => string | null | undefined;
  /** An AsyncLocalStorage store may contain a session ID or an object with sessionId. */
  sessionStorage?: { getStore(): string | { sessionId?: string } | undefined };
  resolveSignal?: (context: ProfileInvocation) => AbortSignal | undefined;
}
export interface ProfileActivationOptions {
  endpoint?: string;
  target?: string;
  /** The existing aw-deployment/v1alpha1 descriptor for the SDK adapter. */
  descriptorFile?: string;
  configFile?: string;
  /** Wait for an attach intent from aw connect --sdk-pid. */
  cooperative?: boolean;
  timeoutMs?: number;
  reconnectMs?: number;
  cwd?: string;
  environment?: Record<string, string>;
  sessionId?: string;
}
export class ProfileSource {
  private constructor();
  readonly profile: Readonly<InstrumentationProfile>;
  readonly hooks: Readonly<Record<string, ProfileHook>>;
  readonly packageVersion: string;
  readonly state:
    | "probed"
    | "installed"
    | "verified"
    | "activating"
    | "active"
    | "poisoned"
    | "draining"
    | "closed";
  readonly activation_epoch: number;
  static probe(
    profile: InstrumentationProfile | string | Uint8Array,
    options?: ProfileProbeOptions,
  ): Promise<ProfileSource>;
  install(): this;
  verify(): Promise<this>;
  activate(options: ProfileActivationOptions): Promise<this>;
  teardown(timeoutMs?: number): Promise<void>;
}
export const probe: typeof ProfileSource.probe;
/** Hash the original callable's normalized Function.prototype.toString source. */
export function functionCodeSha256(fn: Function): string;
