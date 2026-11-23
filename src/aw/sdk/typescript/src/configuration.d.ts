import type { Availability } from "./index.js";

export interface ProfileDescriptor {
  adapter: string;
  version: string;
  entrypoint: string;
}

export interface ConfigurationResult {
  target: string;
  endpoint: string;
  inputEnabled: boolean;
  sessionStartEnabled?: boolean;
  availability: Availability;
}

export function loadConfiguration(options: {
  configFile: string;
  descriptorFile?: string;
  profile: ProfileDescriptor;
  requiresService?: boolean;
  target?: string;
  executable?: string;
  environment?: Record<string, string>;
}): Promise<ConfigurationResult>;
