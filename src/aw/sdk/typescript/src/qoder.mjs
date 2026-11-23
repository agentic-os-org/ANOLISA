/** Optional compatibility shim delegating to @anolisa/aw-sdk-qoder. */
import { AwError } from "./transport.mjs";

const PACKAGE = "@anolisa/aw-sdk-qoder";
const isMissingPackage = (error) =>
  error?.code === "ERR_MODULE_NOT_FOUND" &&
  typeof error.message === "string" &&
  (error.message.includes(`'${PACKAGE}'`) ||
    error.message.includes(`"${PACKAGE}"`));

function missingAdapter(cause) {
  return new AwError("optional_adapter_not_installed", {
    cause,
    message:
      `Optional Qoder adapter package '${PACKAGE}' is not installed. ` +
      `Install '${PACKAGE}' alongside '@anolisa/aw-sdk' or import directly from '${PACKAGE}'.`,
  });
}

// Resolve ESM when the legacy subpath is imported so qoderOptions stays synchronous.
async function loadAdapter() {
  try {
    return await import(PACKAGE);
  } catch (error) {
    if (!isMissingPackage(error)) throw error;
    /* DEV_FALLBACK_START */
    return import("../../typescript-adapters/qoder/src/index.mjs");
    /* DEV_FALLBACK_END */
    const missing = () => {
      throw missingAdapter(error);
    };
    return { qoderOptions: missing, createQoderBridge: missing };
  }
}

const adapter = await loadAdapter();

export function qoderOptions(task, options) {
  return adapter.qoderOptions(task, options);
}

export async function createQoderBridge(options) {
  return adapter.createQoderBridge(options);
}
