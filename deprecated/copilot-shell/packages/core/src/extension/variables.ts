/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { type VariableSchema, VARIABLE_SCHEMA } from './variableSchema.js';
import path from 'node:path';
import * as fs from 'node:fs';
import { QWEN_DIR } from '../config/storage.js';

export const EXTENSIONS_DIRECTORY_NAME = path.join(QWEN_DIR, 'extensions');
/** Legacy extension config filename, kept for backward compatibility. */
export const EXTENSIONS_CONFIG_FILENAME = 'qwen-extension.json';
/** Primary extension config filename (new). */
export const COSH_EXTENSION_CONFIG_FILENAME = 'cosh-extension.json';
export const INSTALL_METADATA_FILENAME = '.qwen-extension-install.json';
export const EXTENSION_SETTINGS_FILENAME = '.env';

/**
 * Resolves the extension config file path within a directory.
 * Prefers `cosh-extension.json`; falls back to `qwen-extension.json` for
 * backward compatibility.
 */
export function findExtensionConfigFilename(dir: string): string {
  const coshPath = path.join(dir, COSH_EXTENSION_CONFIG_FILENAME);
  if (fs.existsSync(coshPath)) {
    return COSH_EXTENSION_CONFIG_FILENAME;
  }
  return EXTENSIONS_CONFIG_FILENAME;
}

export type JsonObject = { [key: string]: JsonValue };
export type JsonArray = JsonValue[];
export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonObject
  | JsonArray;

export type VariableContext = {
  [key in keyof typeof VARIABLE_SCHEMA]?: string;
};

export function validateVariables(
  variables: VariableContext,
  schema: VariableSchema,
) {
  for (const key in schema) {
    const definition = schema[key];
    if (definition.required && !variables[key as keyof VariableContext]) {
      throw new Error(`Missing required variable: ${key}`);
    }
  }
}

export function hydrateString(str: string, context: VariableContext): string {
  validateVariables(context, VARIABLE_SCHEMA);
  const regex = /\${(.*?)}/g;
  return str.replace(regex, (match, key) =>
    context[key as keyof VariableContext] == null
      ? match
      : (context[key as keyof VariableContext] as string),
  );
}

export function recursivelyHydrateStrings(
  obj: JsonValue,
  values: VariableContext,
): JsonValue {
  if (typeof obj === 'string') {
    return hydrateString(obj, values);
  }
  if (Array.isArray(obj)) {
    return obj.map((item) => recursivelyHydrateStrings(item, values));
  }
  if (typeof obj === 'object' && obj !== null) {
    const newObj: JsonObject = {};
    for (const key in obj) {
      if (Object.prototype.hasOwnProperty.call(obj, key)) {
        newObj[key] = recursivelyHydrateStrings(obj[key], values);
      }
    }
    return newObj;
  }
  return obj;
}
