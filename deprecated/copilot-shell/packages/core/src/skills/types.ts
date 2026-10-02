/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Represents the storage level for a skill configuration.
 * - 'project': Stored in `.copilot-shell/skills/` within the project directory
 * - 'custom': User-defined custom skill directories from settings.json
 * - 'user': Stored in `~/.copilot-shell/skills/` in the user's home directory
 * - 'extension': Provided by an installed extension
 * - 'system': Stored in `/usr/share/anolisa/skills`
 */
export type SkillLevel = 'project' | 'custom' | 'user' | 'extension' | 'system';

/**
 * Skill-OS Layer types - 反映 os 分层（skill 的范围和权限）
 * - 'application'
 * - 'runtime'
 * - 'system'
 * - 'core'
 */
export type SkillLayer = 'application' | 'runtime' | 'system' | 'core';

/**
 * Skill-OS Lifecycle phases.
 * - 'meta'
 * - 'production'
 * - 'maintenance'
 * - 'operations'
 * - 'usage'
 */
export type SkillLifecycle =
  | 'meta'
  | 'production'
  | 'maintenance'
  | 'operations'
  | 'usage';

/**
 * Skill status.
 */
export type SkillStatus = 'stable' | 'beta' | 'deprecated' | 'placeholder';

/**
 * Extended skill metadata following Skill-OS specification.
 */
export interface SkillOSMetadata {
  /** Version string (semver) */
  version?: string;

  /** Technology layer */
  layer?: SkillLayer;

  /** Business category */
  category?: string;

  /** Lifecycle phase */
  lifecycle?: SkillLifecycle;

  /** Tags for discovery */
  tags?: string[];

  /** Skill status */
  status?: SkillStatus;

  /** Dependencies (Python/System) */
  dependencies?: string[];
}

/**
 * Core configuration for a skill as stored in SKILL.md files.
 * Each skill directory contains a SKILL.md file with YAML frontmatter
 * containing metadata, followed by markdown content describing the skill.
 */
export interface SkillConfig {
  /** Unique name identifier for the skill */
  name: string;

  /** Human-readable description of what this skill provides */
  description: string;

  /**
   * Optional list of tool names that this skill is allowed to use.
   * For v1, this is informational only (no gating).
   */
  allowedTools?: string[];

  /**
   * Storage level - determines where the configuration file is stored
   */
  level: SkillLevel;

  /**
   * Absolute path to the skill directory containing SKILL.md
   */
  filePath: string;

  /**
   * The markdown body content from SKILL.md (after the frontmatter)
   */
  body: string;

  /**
   * For extension-level skills: the name of the providing extension
   */
  extensionName?: string;

  /**
   * Skill path in remote registry (e.g., "system/network/firewall")
   */
  remotePath?: string;

  /**
   * Extended Skill-OS metadata
   */
  osMetadata?: SkillOSMetadata;

  /**
   * Whether this skill is from remote registry
   */
  isRemote?: boolean;
}

/**
 * Runtime configuration for a skill when it's being actively used.
 * Extends SkillConfig with additional runtime-specific fields.
 */
export type SkillRuntimeConfig = SkillConfig;

/**
 * Result of a validation operation on a skill configuration.
 */
export interface SkillValidationResult {
  /** Whether the configuration is valid */
  isValid: boolean;

  /** Array of error messages if validation failed */
  errors: string[];

  /** Array of warning messages (non-blocking issues) */
  warnings: string[];
}

/**
 * Options for listing skills.
 */
export interface ListSkillsOptions {
  /** Filter by storage level */
  level?: SkillLevel;

  /** Force refresh from disk, bypassing cache. Defaults to false. */
  force?: boolean;

  /** Include remote skills in listing */
  includeRemote?: boolean;

  /**
   * Exclude disabled skills from the result.
   * When true, disabled skills are skipped AND not added to the dedup set,
   * allowing a lower-priority enabled instance of the same skill to surface
   * (priority fallback).
   */
  excludeDisabled?: boolean;
}

/**
 * Error thrown when a skill operation fails.
 */
export class SkillError extends Error {
  constructor(
    message: string,
    readonly code: SkillErrorCode,
    readonly skillName?: string,
  ) {
    super(message);
    this.name = 'SkillError';
  }
}

/**
 * Error codes for skill operations.
 */
export const SkillErrorCode = {
  NOT_FOUND: 'NOT_FOUND',
  INVALID_CONFIG: 'INVALID_CONFIG',
  INVALID_NAME: 'INVALID_NAME',
  FILE_ERROR: 'FILE_ERROR',
  PARSE_ERROR: 'PARSE_ERROR',
} as const;

export type SkillErrorCode =
  (typeof SkillErrorCode)[keyof typeof SkillErrorCode];

/**
 * 远程 skill-os 提供的skill
 */
export interface RemoteSkillEntry {
  /** Skill path in registry (e.g., "system/network/firewall") */
  path: string;

  /** Skill name */
  name: string;

  /** Version string */
  version: string;

  /** Skill description */
  description: string;

  /** Technology layer */
  layer: SkillLayer;

  /** Lifecycle phase */
  lifecycle: SkillLifecycle;

  /** Tags for discovery */
  tags: string[];

  /** Skill status */
  status: SkillStatus;

  /** Dependencies */
  dependencies: string[];
}
