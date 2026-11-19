/** Optional compatibility shim delegating to @anolisa/aw-sdk-pi. */
import { AwError } from "./transport.mjs";

const PACKAGE = "@anolisa/aw-sdk-pi";
const isMissingPackage = (error) =>
  error?.code === "ERR_MODULE_NOT_FOUND" &&
  typeof error.message === "string" &&
  (error.message.includes(`'${PACKAGE}'`) ||
    error.message.includes(`"${PACKAGE}"`));

function missingAdapter(cause) {
  return new AwError("optional_adapter_not_installed", {
    cause,
    message:
      `Optional Pi adapter package '${PACKAGE}' is not installed. ` +
      `Install '${PACKAGE}' alongside '@anolisa/aw-sdk' or import runPiWorker directly from '${PACKAGE}'.`,
  });
}

async function loadOptional() {
  try {
    return await import(PACKAGE);
  } catch (error) {
    if (!isMissingPackage(error)) throw error;
    /* DEV_FALLBACK_START */
    return import("../../typescript-adapters/pi/src/index.mjs");
    /* DEV_FALLBACK_END */
    throw missingAdapter(error);
  }
}

export async function runPiWorker(options) {
  const adapter = await loadOptional();
  return adapter.runPiWorker(options);
}
