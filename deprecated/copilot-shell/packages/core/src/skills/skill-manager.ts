/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'fs/promises';
import * as fsSync from 'fs';
import * as path from 'path';
import * as os from 'os';
import { watch as watchFs, type FSWatcher } from 'chokidar';
import { parse as parseYaml } from '../utils/yaml-parser.js';
import type {
  SkillConfig,
  SkillLevel,
  ListSkillsOptions,
  SkillValidationResult,
} from './types.js';
import { SkillError, SkillErrorCode } from './types.js';
import type { Config } from '../config/config.js';
import { Storage } from '../config/storage.js';
import { validateConfig } from './skill-load.js';
import {
  RemoteSkillRegistry,
  remoteEntryToSkillConfig,
} from './remote-skill-registry.js';
import { SkillsStateManager } from './skills-state.js';

const COPILOT_CONFIG_DIR = '.copilot-shell';
const SKILLS_CONFIG_DIR = 'skills';
const SKILL_MANIFEST_FILE = 'SKILL.md';

/**
 * Manages skill configurations stored as directories containing SKILL.md files.
 * Provides discovery, parsing, validation, and caching for skills.
 */
export class SkillManager {
  private skillsCache: Map<SkillLevel, SkillConfig[]> | null = null;
  private readonly changeListeners: Set<() => void> = new Set();
  private parseErrors: Map<string, SkillError> = new Map();
  private readonly watchers: Map<string, FSWatcher> = new Map();
  private watchStarted = false;
  private refreshTimer: NodeJS.Timeout | null = null;
  private remoteRegistry: RemoteSkillRegistry | null = null;
  private readonly skillsStateManager = new SkillsStateManager();

  constructor(private readonly config: Config) {
    // Initialize remote registry if configured
    this.initRemoteRegistry();
  }

  /**
   * Initialize remote skill registry based on configuration.
   * Called during construction and can be called again if config changes.
   */
  private initRemoteRegistry(): void {
    const remoteUrl = this.config.getSkillOSUrl?.();
    if (remoteUrl && this.config.isRemoteSkillsEnabled?.()) {
      this.remoteRegistry = new RemoteSkillRegistry({
        baseUrl: remoteUrl,
        cacheTTL: this.config.getSkillOSCacheTTL?.() ?? 3600000,
      });
    } else {
      this.remoteRegistry = null;
    }
  }

  /**
   * Adds a listener that will be called when skills change.
   * @returns A function to remove the listener.
   */
  addChangeListener(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  /**
   * Notifies all registered change listeners.
   */
  private notifyChangeListeners(): void {
    for (const listener of this.changeListeners) {
      try {
        listener();
      } catch (error) {
        console.warn('Skill change listener threw an error:', error);
      }
    }
  }

  /**
   * Publicly signals that the effective skill set has changed
   * (e.g. after toggling a skill's disabled state).
   */
  notifySkillsChanged(): void {
    this.notifyChangeListeners();
  }

  /**
   * Gets any parse errors that occurred during skill loading.
   * @returns Map of skill paths to their parse errors.
   */
  getParseErrors(): Map<string, SkillError> {
    return new Map(this.parseErrors);
  }

  /**
   * Lists all available skills.
   *
   * @param options - Filtering options
   * @returns Array of skill configurations
   */
  async listSkills(options: ListSkillsOptions = {}): Promise<SkillConfig[]> {
    const skills: SkillConfig[] = [];
    const seenNames = new Set<string>();

    const levelsToCheck: SkillLevel[] = options.level
      ? [options.level]
      : ['project', 'custom', 'user', 'extension', 'system'];

    // Check if we should use cache or force refresh
    const shouldUseCache = !options.force && this.skillsCache !== null;

    // Initialize cache if it doesn't exist or we're forcing a refresh
    if (!shouldUseCache) {
      // Suppress notification when refresh is triggered from listSkills:
      // the caller (typically SkillTool / TaskTool) is already in the middle
      // of consuming the new state, so a re-entrant notify would just queue
      // a redundant refreshSkills() round-trip.
      await this.refreshCache(false);
    }

    // Pre-load disabled state once when excludeDisabled is requested
    const disabledByLevel = options.excludeDisabled
      ? await this.skillsStateManager.getDisabledSkillsByLevel()
      : null;

    // Collect skills from each level (project takes precedence over user over extension over system)
    for (const level of levelsToCheck) {
      const levelSkills = this.skillsCache?.get(level) || [];

      for (const skill of levelSkills) {
        // Skip if we've already seen this name (precedence: project > user > extension)
        if (seenNames.has(skill.name)) {
          continue;
        }

        // When excludeDisabled is set: skip disabled skills WITHOUT adding to
        // seenNames, so a lower-priority enabled instance can still surface.
        if (disabledByLevel && disabledByLevel[level]?.includes(skill.name)) {
          continue;
        }

        skills.push(skill);
        seenNames.add(skill.name);
      }
    }

    // Include remote skills if requested
    if (options.includeRemote && this.remoteRegistry) {
      try {
        const remoteSkills = await this.listRemoteSkills();
        for (const skill of remoteSkills) {
          // Skip if we've already seen this name (local takes precedence)
          if (!seenNames.has(skill.name)) {
            skills.push(skill);
            seenNames.add(skill.name);
          }
        }
      } catch (error) {
        console.warn('Failed to fetch remote skills:', error);
      }
    }
    // Sort by name for consistent ordering
    skills.sort((a, b) => a.name.localeCompare(b.name));

    return skills;
  }

  /**
   * Lists available remote skills from Skill-OS
   *
   * @returns Array of remote skill configurations (lightweight, without body)
   */
  async listRemoteSkills(): Promise<SkillConfig[]> {
    if (!this.remoteRegistry) {
      return [];
    }

    try {
      const entries = await this.remoteRegistry.fetchIndex();
      return entries.map(remoteEntryToSkillConfig);
    } catch (error) {
      console.warn('Failed to fetch remote skills:', error);
      return [];
    }
  }

  /**
   * Loads a skill configuration by name.
   * If level is specified, only searches that level.
   * If level is omitted, searches project-level first, then user-level, then extension, then system.
   *
   * @param name - Name of the skill to load
   * @param level - Optional level to limit search to
   * @param options - Additional options
   * @returns SkillConfig or null if not found
   */
  async loadSkill(
    name: string,
    level?: SkillLevel,
    options?: { excludeDisabled?: boolean },
  ): Promise<SkillConfig | null> {
    if (level) {
      const skill = await this.findSkillByNameAtLevel(name, level);
      if (skill && options?.excludeDisabled) {
        const disabled = await this.skillsStateManager.isDisabled(name, level);
        if (disabled) {
          return null;
        }
      }
      return skill;
    }

    const levelsInOrder: SkillLevel[] = [
      'project',
      'custom',
      'user',
      'extension',
      'system',
    ];

    for (const lvl of levelsInOrder) {
      const skill = await this.findSkillByNameAtLevel(name, lvl);
      if (skill) {
        // When excludeDisabled, skip disabled levels (continue searching lower priority)
        if (options?.excludeDisabled) {
          const disabled = await this.skillsStateManager.isDisabled(name, lvl);
          if (disabled) {
            continue;
          }
        }
        return skill;
      }
    }

    // Fall back to remote
    return this.loadRemoteSkill(name);
  }

  /**
   * Load a skill from remote registry.
   *
   * @param name - Name of the skill to load
   * @returns SkillConfig or null if not found
   */
  async loadRemoteSkill(name: string): Promise<SkillConfig | null> {
    if (!this.remoteRegistry) {
      return null;
    }

    try {
      // Find skill in index
      const skills = await this.remoteRegistry.fetchIndex();
      const entry = skills.find((s) => s.name === name);
      if (!entry) {
        return null;
      }

      // Download and parse
      return this.remoteRegistry.downloadSkill(entry.path);
    } catch (error) {
      console.warn(`Failed to load remote skill ${name}:`, error);
      return null;
    }
  }

  /**
   * Clear remote skill cache.
   */
  async clearRemoteCache(): Promise<void> {
    await this.remoteRegistry?.clearCache();
  }

  /**
   * Get the remote skill registry instance.
   */
  getRemoteRegistry(): RemoteSkillRegistry | null {
    return this.remoteRegistry;
  }

  /**
   * Get the skills state manager instance for enable/disable persistence.
   */
  getSkillsStateManager(): SkillsStateManager {
    return this.skillsStateManager;
  }

  /**
   * Loads a skill with its full content, ready for runtime use.
   * This includes loading additional files from the skill directory.
   *
   * @param name - Name of the skill to load
   * @param level - Optional level to limit search to
   * @returns SkillConfig or null if not found
   */
  async loadSkillForRuntime(
    name: string,
    level?: SkillLevel,
  ): Promise<SkillConfig | null> {
    const skill = await this.loadSkill(name, level);
    if (!skill) {
      return null;
    }

    return skill;
  }

  /**
   * Validates a skill configuration.
   *
   * @param config - Configuration to validate
   * @returns Validation result
   */
  validateConfig(config: Partial<SkillConfig>): SkillValidationResult {
    return validateConfig(config);
  }

  /**
   * Refreshes the skills cache by loading all skills from disk.
   *
   * @param notify - When true (default), invokes registered change listeners
   *   after the cache is rebuilt. Pass `false` for internal call sites that
   *   are themselves reacting to a listener invocation, to avoid re-entrant
   *   refresh storms.
   */
  async refreshCache(notify: boolean = true): Promise<void> {
    const skillsCache = new Map<SkillLevel, SkillConfig[]>();
    this.parseErrors.clear();

    const levels: SkillLevel[] = [
      'project',
      'custom',
      'user',
      'extension',
      'system',
    ];

    // Scan levels sequentially. They touch disjoint directories, but tests
    // assert deterministic readdir ordering, and per-level latency is small
    // enough that parallelism is not worth the contract change.
    for (const level of levels) {
      const levelSkills = await this.listSkillsAtLevel(level);
      skillsCache.set(level, levelSkills);
    }

    this.skillsCache = skillsCache;
    if (notify) {
      this.notifyChangeListeners();
    }
  }

  /**
   * Starts watching skill directories for changes.
   */
  async startWatching(): Promise<void> {
    if (this.watchStarted) {
      return;
    }

    this.watchStarted = true;
    await this.ensureUserSkillsDir();
    await this.refreshCache();
    this.updateWatchersFromCache();
  }

  /**
   * Stops watching skill directories for changes.
   */
  stopWatching(): void {
    for (const watcher of this.watchers.values()) {
      void watcher.close().catch((error) => {
        console.warn('Failed to close skills watcher:', error);
      });
    }
    this.watchers.clear();
    this.watchStarted = false;
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
  }

  /**
   * Parses a SKILL.md file and returns the configuration.
   *
   * @param filePath - Path to the SKILL.md file
   * @param level - Storage level
   * @returns SkillConfig
   * @throws SkillError if parsing fails
   */
  parseSkillFile(filePath: string, level: SkillLevel): Promise<SkillConfig> {
    return this.parseSkillFileInternal(filePath, level);
  }

  /**
   * Internal implementation of skill file parsing.
   */
  private async parseSkillFileInternal(
    filePath: string,
    level: SkillLevel,
  ): Promise<SkillConfig> {
    let content: string;

    try {
      content = await fs.readFile(filePath, 'utf8');
    } catch (error) {
      const skillError = new SkillError(
        `Failed to read skill file: ${error instanceof Error ? error.message : 'Unknown error'}`,
        SkillErrorCode.FILE_ERROR,
      );
      this.parseErrors.set(filePath, skillError);
      throw skillError;
    }

    return this.parseSkillContent(content, filePath, level);
  }

  /**
   * Parses skill content from a string.
   *
   * @param content - File content
   * @param filePath - File path for error reporting
   * @param level - Storage level
   * @returns SkillConfig
   * @throws SkillError if parsing fails
   */
  parseSkillContent(
    content: string,
    filePath: string,
    level: SkillLevel,
  ): SkillConfig {
    try {
      const normalizedContent = normalizeSkillFileContent(content);

      // Split frontmatter and content
      const frontmatterRegex = /^---\n([\s\S]*?)\n---(?:\n|$)([\s\S]*)$/;
      const match = normalizedContent.match(frontmatterRegex);

      if (!match) {
        throw new Error('Invalid format: missing YAML frontmatter');
      }

      const [, frontmatterYaml, body] = match;

      // Parse YAML frontmatter
      const frontmatter = parseYaml(frontmatterYaml) as Record<string, unknown>;

      // Extract required fields
      const nameRaw = frontmatter['name'];
      const descriptionRaw = frontmatter['description'];

      if (nameRaw == null || nameRaw === '') {
        throw new Error('Missing "name" in frontmatter');
      }

      if (descriptionRaw == null || descriptionRaw === '') {
        throw new Error('Missing "description" in frontmatter');
      }

      // Convert to strings
      const name = String(nameRaw);
      const description = String(descriptionRaw)
        .replace(/\s*\n\s*/g, ' ')
        .trim();

      // Extract optional fields
      const allowedToolsRaw = frontmatter['allowedTools'] as
        | unknown[]
        | undefined;
      let allowedTools: string[] | undefined;

      if (allowedToolsRaw !== undefined) {
        if (Array.isArray(allowedToolsRaw)) {
          allowedTools = allowedToolsRaw.map(String);
        } else {
          throw new Error('"allowedTools" must be an array');
        }
      }

      const config: SkillConfig = {
        name,
        description,
        allowedTools,
        level,
        filePath,
        body: body.trim(),
      };

      // Validate the parsed configuration
      const validation = this.validateConfig(config);
      if (!validation.isValid) {
        throw new Error(`Validation failed: ${validation.errors.join(', ')}`);
      }

      return config;
    } catch (error) {
      const skillError = new SkillError(
        `Failed to parse skill file: ${error instanceof Error ? error.message : 'Unknown error'}`,
        SkillErrorCode.PARSE_ERROR,
      );
      this.parseErrors.set(filePath, skillError);
      throw skillError;
    }
  }

  /**
   * Gets the base directory for skills at a specific level.
   *
   * @param level - Storage level
   * @returns Absolute directory path
   */
  getSkillsBaseDir(level: SkillLevel): string {
    if (level === 'system') {
      return Storage.getSystemSkillsDir();
    }

    // Custom level: return first custom dir, or empty string
    if (level === 'custom') {
      const customDirs = this.config.getResolvedCustomSkillPaths();
      return customDirs[0] ?? '';
    }

    const baseDir =
      level === 'project'
        ? path.join(
            this.config.getProjectRoot(),
            COPILOT_CONFIG_DIR,
            SKILLS_CONFIG_DIR,
          )
        : path.join(os.homedir(), COPILOT_CONFIG_DIR, SKILLS_CONFIG_DIR);

    return baseDir;
  }

  /**
   * Lists skills at a specific level.
   *
   * @param level - Storage level to scan
   * @returns Array of skill configurations
   */
  // TODO(auto-memory): Auto Memory extraction writes skills to
  // config.storage.getProjectSkillsMemoryDir() (~/.copilot-shell/tmp/{hash}/memory/skills/).
  // This directory is not included in any scan level below.
  // To close the gap, add a scan of getProjectSkillsMemoryDir() when level === 'project'
  // (or introduce a new 'auto-memory' level). Also add an FSWatcher for that directory.
  private async listSkillsAtLevel(level: SkillLevel): Promise<SkillConfig[]> {
    const projectRoot = this.config.getProjectRoot();
    const homeDir = os.homedir();
    const isHomeDirectory = path.resolve(projectRoot) === path.resolve(homeDir);

    // If project level is requested but project root is same as home directory,
    // return empty array to avoid conflicts between project and global skills
    if (level === 'project' && isHomeDirectory) {
      return [];
    }

    // Custom level: scan all user-defined custom directories
    if (level === 'custom') {
      const customDirs = this.config.getResolvedCustomSkillPaths();
      const allSkills: SkillConfig[] = [];
      for (const dir of customDirs) {
        const skills = await this.loadSkillsFromDir(dir, 'custom');
        allSkills.push(...skills);
      }
      return allSkills;
    }

    if (level === 'extension') {
      const extensions = this.config.getActiveExtensions();
      const skills: SkillConfig[] = [];
      for (const extension of extensions) {
        extension.skills?.forEach((skill) => {
          skills.push(skill);
        });
      }

      return skills;
    }

    const baseDir = this.getSkillsBaseDir(level);
    const skills = await this.loadSkillsFromDir(baseDir, level);
    return skills;
  }

  async loadSkillsFromDir(
    baseDir: string,
    level: SkillLevel,
  ): Promise<SkillConfig[]> {
    try {
      const entries = await fs.readdir(baseDir, { withFileTypes: true });
      const skills: SkillConfig[] = [];
      for (const entry of entries) {
        // Only process directories (each skill is a directory)
        if (!entry.isDirectory()) continue;

        const skillDir = path.join(baseDir, entry.name);
        const skillManifest = path.join(skillDir, SKILL_MANIFEST_FILE);

        try {
          // Check if SKILL.md exists
          await fs.access(skillManifest);

          const config = await this.parseSkillFileInternal(
            skillManifest,
            level,
          );
          skills.push(config);
        } catch (error) {
          // Skip directories without valid SKILL.md
          if (error instanceof SkillError) {
            // Parse error was already recorded
            console.warn(
              `Failed to parse skill at ${skillDir}: ${error.message}`,
            );
          }
          continue;
        }
      }
      return skills;
    } catch (_error) {
      // Directory doesn't exist or can't be read
      return [];
    }
  }

  /**
   * Finds a skill by name at a specific level.
   *
   * @param name - Name of the skill to find
   * @param level - Storage level to search
   * @returns SkillConfig or null if not found
   */
  private async findSkillByNameAtLevel(
    name: string,
    level: SkillLevel,
  ): Promise<SkillConfig | null> {
    await this.ensureLevelCache(level);

    const levelSkills = this.skillsCache?.get(level) || [];

    // Find the skill with matching name
    return levelSkills.find((skill) => skill.name === name) || null;
  }

  /**
   * Ensures the cache is populated for a specific level without loading other levels.
   */
  private async ensureLevelCache(level: SkillLevel): Promise<void> {
    if (!this.skillsCache) {
      this.skillsCache = new Map<SkillLevel, SkillConfig[]>();
    }

    if (!this.skillsCache.has(level)) {
      const levelSkills = await this.listSkillsAtLevel(level);
      this.skillsCache.set(level, levelSkills);
    }
  }

  private updateWatchersFromCache(): void {
    const watchTargets = new Set<string>(
      (['project', 'user', 'system'] as const)
        .map((level) => this.getSkillsBaseDir(level))
        .filter((baseDir) => fsSync.existsSync(baseDir)),
    );

    // Also watch custom skill directories
    for (const dir of this.config.getResolvedCustomSkillPaths()) {
      if (fsSync.existsSync(dir)) {
        watchTargets.add(dir);
      }
    }

    for (const existingPath of this.watchers.keys()) {
      if (!watchTargets.has(existingPath)) {
        void this.watchers
          .get(existingPath)
          ?.close()
          .catch((error) => {
            console.warn(
              `Failed to close skills watcher for ${existingPath}:`,
              error,
            );
          });
        this.watchers.delete(existingPath);
      }
    }

    for (const watchPath of watchTargets) {
      if (this.watchers.has(watchPath)) {
        continue;
      }

      try {
        const watcher = watchFs(watchPath, {
          ignoreInitial: true,
        })
          .on('all', () => {
            this.scheduleRefresh();
          })
          .on('error', (error) => {
            console.warn(`Skills watcher error for ${watchPath}:`, error);
          });
        this.watchers.set(watchPath, watcher);
      } catch (error) {
        console.warn(
          `Failed to watch skills directory at ${watchPath}:`,
          error,
        );
      }
    }
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }

    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = null;
      void this.refreshCache().then(() => this.updateWatchersFromCache());
    }, 150);
  }

  private async ensureUserSkillsDir(): Promise<void> {
    const baseDir = this.getSkillsBaseDir('user');
    try {
      await fs.mkdir(baseDir, { recursive: true });
    } catch (error) {
      console.warn(
        `Failed to create user skills directory at ${baseDir}:`,
        error,
      );
    }
  }
}

function normalizeSkillFileContent(content: string): string {
  // Strip UTF-8 BOM to ensure frontmatter starts at the first character.
  let normalized = content.replace(/^\uFEFF/, '');

  // Normalize line endings so skills authored on Windows (CRLF) parse correctly.
  normalized = normalized.replace(/\r\n/g, '\n').replace(/\r/g, '\n');

  return normalized;
}
