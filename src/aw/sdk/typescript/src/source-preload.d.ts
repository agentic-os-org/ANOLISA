import type { Availability } from "./index.js";
import type { SourceEvent } from "./sources.js";

export interface FetchAutoTarget {
  kind: "fetch";
  name?: string;
  maxBodyBytes?: number;
}

export interface ModuleAutoTarget {
  kind: "module";
  module: string;
  exportName: string;
  role?: "tool" | "input";
  name?: string;
  bufferStream?: boolean;
  maxStreamItems?: number;
}

export type AutoTarget = FetchAutoTarget | ModuleAutoTarget;

export interface AutoPreloadSpec {
  endpoint: string;
  target: string;
  availability?: Availability;
  sessionId?: string;
  events?: readonly SourceEvent[];
  replaceInput?: boolean;
  replaceResult?: boolean;
  withholdResult?: boolean;
  timeoutMs?: number;
  cwd?: string;
  targets: readonly AutoTarget[];
}

export class AutoPreload {
  readonly spec: AutoPreloadSpec;
  readonly active: boolean;
  uninstall(): Promise<void>;
}

export function validateAutoSpec(raw: unknown): AutoPreloadSpec;
export function loadSpecFromEnv(
  env?: NodeJS.ProcessEnv,
): AutoPreloadSpec | null;
export function installAutoInstrumentation(spec: AutoPreloadSpec): AutoPreload;
export function installFromEnv(env?: NodeJS.ProcessEnv): AutoPreload | null;
export function getActivePreload(): AutoPreload | null;
