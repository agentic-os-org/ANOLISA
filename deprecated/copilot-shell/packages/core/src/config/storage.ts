/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import * as path from 'node:path';
import * as os from 'node:os';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import { resolveEnvVarsInString } from '../utils/envVarResolver.js';

export const QWEN_DIR = '.copilot-shell';
export const GOOGLE_ACCOUNTS_FILENAME = 'google_accounts.json';
export const OAUTH_FILE = 'oauth_creds.json';
const TMP_DIR_NAME = 'tmp';
const BIN_DIR_NAME = 'bin';
const PROJECT_DIR_NAME = 'projects';
const IDE_DIR_NAME = 'ide';
const DEBUG_DIR_NAME = 'debug';

export class Storage {
  private readonly targetDir: string;

  constructor(targetDir: string) {
    this.targetDir = targetDir;
  }

  static getGlobalQwenDir(): string {
    const homeDir = os.homedir();
    if (!homeDir) {
      return path.join(os.tmpdir(), '.copilot-shell');
    }
    return path.join(homeDir, QWEN_DIR);
  }

  static getMcpOAuthTokensPath(): string {
    return path.join(Storage.getGlobalQwenDir(), 'mcp-oauth-tokens.json');
  }

  static getGlobalSettingsPath(): string {
    return path.join(Storage.getGlobalQwenDir(), 'settings.json');
  }

  static getInstallationIdPath(): string {
    return path.join(Storage.getGlobalQwenDir(), 'installation_id');
  }

  static getGoogleAccountsPath(): string {
    return path.join(Storage.getGlobalQwenDir(), GOOGLE_ACCOUNTS_FILENAME);
  }

  static getUserCommandsDir(): string {
    return path.join(Storage.getGlobalQwenDir(), 'commands');
  }

  static getGlobalMemoryFilePath(): string {
    return path.join(Storage.getGlobalQwenDir(), 'memory.md');
  }

  static getGlobalTempDir(): string {
    return path.join(Storage.getGlobalQwenDir(), TMP_DIR_NAME);
  }

  static getGlobalIdeDir(): string {
    return path.join(Storage.getGlobalQwenDir(), IDE_DIR_NAME);
  }

  static getGlobalBinDir(): string {
    return path.join(Storage.getGlobalQwenDir(), BIN_DIR_NAME);
  }

  static getGlobalDebugDir(): string {
    return path.join(Storage.getGlobalQwenDir(), DEBUG_DIR_NAME);
  }

  static getDebugLogPath(sessionId: string): string {
    return path.join(Storage.getGlobalDebugDir(), `${sessionId}.log`);
  }

  getQwenDir(): string {
    return path.join(this.targetDir, QWEN_DIR);
  }

  getProjectDir(): string {
    const projectId = this.sanitizeCwd(this.getProjectRoot());
    const projectsDir = path.join(Storage.getGlobalQwenDir(), PROJECT_DIR_NAME);
    return path.join(projectsDir, projectId);
  }

  getProjectTempDir(): string {
    const hash = this.getFilePathHash(this.getProjectRoot());
    const tempDir = Storage.getGlobalTempDir();
    return path.join(tempDir, hash);
  }

  ensureProjectTempDirExists(): void {
    fs.mkdirSync(this.getProjectTempDir(), { recursive: true });
  }

  static getOAuthCredsPath(): string {
    return path.join(Storage.getGlobalQwenDir(), OAUTH_FILE);
  }

  getProjectRoot(): string {
    return this.targetDir;
  }

  private getFilePathHash(filePath: string): string {
    return crypto.createHash('sha256').update(filePath).digest('hex');
  }

  getHistoryDir(): string {
    const hash = this.getFilePathHash(this.getProjectRoot());
    const historyDir = path.join(Storage.getGlobalQwenDir(), 'history');
    return path.join(historyDir, hash);
  }

  getWorkspaceSettingsPath(): string {
    return path.join(this.getQwenDir(), 'settings.json');
  }

  getProjectCommandsDir(): string {
    return path.join(this.getQwenDir(), 'commands');
  }

  getProjectTempCheckpointsDir(): string {
    return path.join(this.getProjectTempDir(), 'checkpoints');
  }

  getProjectMemoryTempDir(): string {
    return path.join(this.getProjectTempDir(), 'memory');
  }

  getProjectSkillsMemoryDir(): string {
    return path.join(this.getProjectMemoryTempDir(), 'skills');
  }

  getExtensionsDir(): string {
    return path.join(this.getQwenDir(), 'extensions');
  }

  getExtensionsConfigPath(): string {
    return path.join(this.getExtensionsDir(), 'qwen-extension.json');
  }

  getUserSkillsDir(): string {
    return path.join(Storage.getGlobalQwenDir(), 'skills');
  }

  static getSystemSkillsDir(): string {
    return '/usr/share/anolisa/skills';
  }

  static getSkillsStatePath(): string {
    return path.join(Storage.getGlobalQwenDir(), 'skills-state.json');
  }

  getGlobalRemoteSkillsDir(): string {
    return path.join(Storage.getGlobalQwenDir(), 'remote-skills');
  }

  getRemoteSkillsDir(): string {
    const projectRoot = this.getProjectRoot();
    const homeDir = os.homedir();
    const isHomeDirectory = path.resolve(projectRoot) === path.resolve(homeDir);

    return isHomeDirectory
      ? path.join(Storage.getGlobalQwenDir(), 'remote-skills')
      : path.join(this.getQwenDir(), 'remote-skills');
  }

  getHistoryFilePath(): string {
    return path.join(this.getProjectTempDir(), 'shell_history');
  }

  /**
   * Resolves an array of custom skill paths by expanding:
   * - ~ and ~/ to user's home directory
   * - $VAR and ${VAR} to environment variable values
   * Then resolves to absolute paths.
   *
   * Invalid or empty paths after resolution are filtered out.
   */
  resolveCustomSkillPaths(rawPaths: string[]): string[] {
    const homeDir = os.homedir();
    return rawPaths
      .map((p) => {
        let expanded = p.trim();
        if (!expanded) return '';
        // Expand tilde
        if (expanded === '~') {
          expanded = homeDir;
        } else if (expanded.startsWith('~/')) {
          expanded = path.join(homeDir, expanded.slice(2));
        }
        // Expand environment variables ($VAR and ${VAR})
        expanded = resolveEnvVarsInString(expanded);
        // Resolve to absolute path
        return path.resolve(expanded);
      })
      .filter((p) => p !== '');
  }

  private sanitizeCwd(cwd: string): string {
    return cwd.replace(/[^a-zA-Z0-9]/g, '-');
  }
}
