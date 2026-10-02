export class StepError extends Error {
  constructor(stepName, message, step) {
    super(`${stepName}: ${message}`);
    this.name = "StepError";
    this.step = {
      name: step.name,
      command: step.command,
      args: step.args,
      exitCode: step.exitCode,
      signal: step.signal,
      timedOut: step.timedOut,
      stdoutLog: step.stdoutLog,
      stderrLog: step.stderrLog,
    };
  }
}

export function serializeError(error) {
  if (error instanceof StepError) {
    return {
      name: error.name,
      message: error.message,
      step: error.step,
    };
  }
  if (error instanceof Error) {
    return {
      name: error.name,
      message: error.message,
      stack: error.stack,
    };
  }
  return {
    name: typeof error,
    message: String(error),
  };
}

export function formatError(error) {
  if (!error) return "unknown error";
  if (error instanceof StepError) {
    return `${error.message}; stdout=${error.step.stdoutLog} stderr=${error.step.stderrLog}`;
  }
  if (error instanceof Error) return `${error.name}: ${error.message}`;
  return String(error);
}
