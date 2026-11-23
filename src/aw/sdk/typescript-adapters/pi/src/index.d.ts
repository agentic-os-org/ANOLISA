export interface PiWorkerOptions {
  /** Absolute .mjs file exporting createSession and runWorker (or run). */
  application: string;
  args?: string[];
  /** Absolute AW configuration used by the deployment controller. */
  configFile: string;
  /** Trusted `aw config descriptor --profile pi-worker-hooks` output; skips the parser. */
  descriptorFile?: string;
  target?: string;
  executable?: string;
  attachTimeoutMs?: number;
  /** Attach automatically when the configured Service is available (defaults to false). */
  automatic?: boolean;
}

/**
 * Start the application's Pi worker with an explicit SDK attach listener.
 * Strict tool-result output is admitted only for the verified SDK/Core releases.
 */
export function runPiWorker(options: PiWorkerOptions): Promise<void>;
