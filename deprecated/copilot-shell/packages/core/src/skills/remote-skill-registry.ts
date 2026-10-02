/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import * as os from 'os';
import { existsSync } from 'fs';
import type {
  RemoteSkillEntry,
  SkillConfig,
  SkillOSMetadata,
  SkillLayer,
  SkillLifecycle,
  SkillStatus,
} from './types.js';
import { parseSkillContent } from './skill-load.js';

/**
 * Configuration for RemoteSkillRegistry.
 */
export interface RemoteSkillRegistryConfig {
  /** Base URL of Skill-OS API */
  baseUrl: string;

  /** Local cache directory */
  cacheDir?: string;

  /** Index cache TTL in milliseconds (default: 1 hour) */
  cacheTTL?: number;

  /** Request timeout in milliseconds */
  timeout?: number;

  /** Optional auth token (reserved for future use) */
  authToken?: string;
}

/**
 * 搜索技能的查询选项。
 */
export interface SkillSearchQuery {
  /** 在名称和描述中搜索的关键字 */
  keyword?: string;

  /** 按os层级过滤 */
  layer?: SkillLayer;

  /** 按生命周期阶段过滤 */
  lifecycle?: SkillLifecycle;

  /** 按类别过滤 */
  category?: string;

  /** 按标签过滤（技能必须包含至少一个） */
  tags?: string[];

  /** 按状态过滤（技能必须匹配其中之一） */
  status?: SkillStatus[];
}

/**
 * Cached index data structure.
 */
interface IndexCache {
  fetchedAt: number;
  skills: RemoteSkillEntry[];
}

const DEFAULT_CACHE_TTL = 3600000; // 1 hour
const DEFAULT_INDEX_TIMEOUT = 10000; // 10 seconds, used for fast metadata calls
const DEFAULT_DOWNLOAD_TIMEOUT = 60000; // 60 seconds, used for skill package downloads
const COPILOT_CONFIG_DIR = '.copilot-shell';
const REMOTE_SKILLS_DIR = 'remote-skills';
const INDEX_CACHE_FILE = 'index.json';

/**
 * 远程技能注册表 - 处理从 Skill-OS 市场获取技能。
 *
 * 该服务提供：
 * - 从远程 API 获取技能索引
 * - 根据各种条件搜索技能
 * - 下载技能到本地缓存
 * - 管理技能缓存生命周期
 */
export class RemoteSkillRegistry {
  private readonly baseUrl: string;
  private readonly cacheDir: string;
  private readonly cacheTTL: number;
  private readonly indexTimeout: number;
  private readonly downloadTimeout: number;
  private readonly authToken?: string;

  private indexCache: IndexCache | null = null;

  constructor(config: RemoteSkillRegistryConfig) {
    this.baseUrl = config.baseUrl.replace(/\/$/, '');
    this.cacheDir =
      config.cacheDir ??
      path.join(os.homedir(), COPILOT_CONFIG_DIR, REMOTE_SKILLS_DIR);
    this.cacheTTL = config.cacheTTL ?? DEFAULT_CACHE_TTL;
    // When `timeout` is explicitly provided, honour it for both phases to
    // preserve backward compatibility. Otherwise, use a short timeout for
    // metadata calls (index/readme) and a longer one for downloads so that a
    // slow Skill-OS server cannot block the cold-start path for 30s.
    if (config.timeout !== undefined) {
      this.indexTimeout = config.timeout;
      this.downloadTimeout = config.timeout;
    } else {
      this.indexTimeout = DEFAULT_INDEX_TIMEOUT;
      this.downloadTimeout = DEFAULT_DOWNLOAD_TIMEOUT;
    }
    this.authToken = config.authToken;
  }

  /**
   * 从远程服务器获取技能索引。
   *
   * @param force - 如果为 true，则绕过缓存并获取最新数据
   * @returns 远程技能条目数组
   */
  async fetchIndex(force = false): Promise<RemoteSkillEntry[]> {
    // 检查内存缓存
    if (
      !force &&
      this.indexCache &&
      Date.now() - this.indexCache.fetchedAt < this.cacheTTL
    ) {
      return this.indexCache.skills;
    }

    // 检查文件缓存
    if (!force) {
      const fileCache = await this.loadIndexFromFile();
      if (fileCache && Date.now() - fileCache.fetchedAt < this.cacheTTL) {
        this.indexCache = fileCache;
        return fileCache.skills;
      }
    }

    // 从远程获取
    console.debug(
      `[RemoteSkillRegistry] Fetching remote skill index from: ${this.baseUrl}/skills/api/v1/skills`,
    );
    let response: Response;
    try {
      response = await this.fetchWithTimeout(
        `${this.baseUrl}/skills/api/v1/skills`,
      );
    } catch (error) {
      const isAbort = error instanceof Error && error.name === 'AbortError';
      const message = isAbort
        ? `Remote skill server timed out after ${this.indexTimeout}ms. The server may be unreachable.`
        : `Failed to connect to remote skill server: ${error instanceof Error ? error.message : String(error)}`;
      throw new Error(message);
    }

    if (!response.ok) {
      throw new Error(`Failed to fetch skill index: ${response.status}`);
    }

    const contentType = response.headers.get('content-type') ?? '';
    if (!contentType.includes('application/json')) {
      throw new Error(
        `Remote skill server returned non-JSON response (Content-Type: ${contentType || 'unknown'}). ` +
          `The server may be unreachable or requires authentication.`,
      );
    }

    const data = (await response.json()) as {
      total: number;
      skills: RemoteSkillEntry[];
    };

    // 更新缓存
    this.indexCache = {
      fetchedAt: Date.now(),
      skills: data.skills,
    };
    await this.saveIndexToFile(this.indexCache);

    return data.skills;
  }

  /**
   * 按查询条件搜索技能。
   *
   * @param query - 搜索查询选项
   * @returns 过滤后的技能条目数组
   */
  async searchSkills(query: SkillSearchQuery): Promise<RemoteSkillEntry[]> {
    const skills = await this.fetchIndex();

    return skills.filter((skill) => {
      if (query.layer && skill.layer !== query.layer) return false;
      if (query.lifecycle && skill.lifecycle !== query.lifecycle) return false;
      if (query.keyword) {
        const kw = query.keyword.toLowerCase();
        if (
          !skill.name.toLowerCase().includes(kw) &&
          !skill.description.toLowerCase().includes(kw)
        ) {
          return false;
        }
      }
      if (query.tags?.length) {
        if (!query.tags.some((t) => skill.tags.includes(t))) return false;
      }
      if (query.status?.length) {
        if (!query.status.includes(skill.status)) return false;
      }
      return true;
    });
  }

  /**
   * 下载技能到本地缓存并返回 SkillConfig。
   *
   * @param skillPath - 注册表中的技能路径 (例如 "system/network/firewall")
   * @returns 解析后的技能配置
   */
  async downloadSkill(skillPath: string): Promise<SkillConfig> {
    // Check if already cached
    const cached = await this.getCachedSkill(skillPath);
    if (cached) {
      return cached;
    }

    // 创建技能目录
    const skillDir = this.getSkillCacheDir(skillPath);
    await fs.mkdir(skillDir, { recursive: true });

    // Download zip
    const zipPath = path.join(skillDir, 'skill.zip');
    const downloadUrl = `${this.baseUrl}/skills/api/v1/skills/${skillPath}/download`;

    const response = await this.fetchWithTimeout(downloadUrl, {
      timeoutOverride: this.downloadTimeout,
    });
    if (!response.ok) {
      throw new Error(`Failed to download skill: ${response.status}`);
    }

    // Save zip file using arrayBuffer for cross-platform compatibility
    const arrayBuffer = await response.arrayBuffer();
    const buffer = Buffer.from(arrayBuffer);
    await fs.writeFile(zipPath, buffer);

    // Extract zip using built-in decompress
    await this.extractZip(zipPath, skillDir);

    // Remove zip after extraction
    await fs.unlink(zipPath);

    // Flatten if the zip contained a single top-level directory wrapper
    const skillMdPath = path.join(skillDir, 'SKILL.md');
    if (!existsSync(skillMdPath)) {
      const entries = await fs.readdir(skillDir);
      if (entries.length === 1) {
        const wrapperPath = path.join(skillDir, entries[0]);
        const stat = await fs.stat(wrapperPath);
        if (stat.isDirectory()) {
          const wrapperMdPath = path.join(wrapperPath, 'SKILL.md');
          if (existsSync(wrapperMdPath)) {
            // Move everything out in parallel
            const items = await fs.readdir(wrapperPath);
            await Promise.all(
              items.map((item) =>
                fs.rename(
                  path.join(wrapperPath, item),
                  path.join(skillDir, item),
                ),
              ),
            );
            await fs.rm(wrapperPath, { recursive: true, force: true });
          }
        }
      }
    }

    // Parse SKILL.md
    return this.parseDownloadedSkill(skillPath, skillDir);
  }

  /**
   * 获取技能内容 (SKILL.md) 而不下载完整的包。
   *
   * @param skillPath - 注册表中的技能路径
   * @returns SKILL.md 内容字符串
   */
  async getSkillReadme(skillPath: string): Promise<string> {
    const response = await this.fetchWithTimeout(
      `${this.baseUrl}/skills/api/v1/skills/${skillPath}/readme`,
    );

    if (!response.ok) {
      throw new Error(`Failed to fetch skill readme: ${response.status}`);
    }

    const data = (await response.json()) as { path: string; content: string };
    return data.content;
  }

  /**
   * 检查技能是否已缓存在本地。
   *
   * @param skillPath - 注册表中的技能路径
   * @returns 如果技能已缓存则返回 true
   */
  async isSkillCached(skillPath: string): Promise<boolean> {
    const skillMdPath = path.join(this.getSkillCacheDir(skillPath), 'SKILL.md');
    return existsSync(skillMdPath);
  }

  /**
   * 获取缓存的技能配置。
   *
   * @param skillPath - 注册表中的技能路径
   * @returns SkillConfig 或 null（如果未缓存）
   */
  async getCachedSkill(skillPath: string): Promise<SkillConfig | null> {
    const skillDir = this.getSkillCacheDir(skillPath);
    const skillMdPath = path.join(skillDir, 'SKILL.md');

    try {
      const content = await fs.readFile(skillMdPath, 'utf8');
      return this.parseSkillMdContent(content, skillMdPath, skillPath);
    } catch {
      return null;
    }
  }

  /**
   * 清除所有缓存的技能。
   */
  async clearCache(): Promise<void> {
    try {
      await fs.rm(this.cacheDir, { recursive: true, force: true });
      this.indexCache = null;
    } catch {
      // Directory might not exist
    }
  }

  /**
   * 仅清除索引缓存（下次获取时强制刷新）。
   */
  clearIndexCache(): void {
    this.indexCache = null;
  }

  /**
   * 获取注册表的 base URL。
   */
  getBaseUrl(): string {
    return this.baseUrl;
  }

  /**
   * 获取缓存目录路径。
   */
  getCacheDir(): string {
    return this.cacheDir;
  }

  // 辅助方法

  private getSkillCacheDir(skillPath: string): string {
    return path.join(this.cacheDir, skillPath);
  }

  private async fetchWithTimeout(
    url: string,
    options: { timeoutOverride?: number } = {},
  ): Promise<Response> {
    console.debug(`[RemoteSkillRegistry] Fetching: ${url}`);
    const controller = new AbortController();
    const timeoutMs = options.timeoutOverride ?? this.indexTimeout;
    const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

    try {
      const headers: Record<string, string> = {
        Accept: 'application/json',
      };
      if (this.authToken) {
        headers['Authorization'] = `Bearer ${this.authToken}`;
      }

      return await fetch(url, {
        signal: controller.signal,
        headers,
      });
    } finally {
      clearTimeout(timeoutId);
    }
  }

  private async loadIndexFromFile(): Promise<IndexCache | null> {
    const indexPath = path.join(this.cacheDir, INDEX_CACHE_FILE);
    try {
      const content = await fs.readFile(indexPath, 'utf8');
      return JSON.parse(content) as IndexCache;
    } catch {
      return null;
    }
  }

  private async saveIndexToFile(cache: IndexCache): Promise<void> {
    await fs.mkdir(this.cacheDir, { recursive: true });
    const indexPath = path.join(this.cacheDir, INDEX_CACHE_FILE);
    await fs.writeFile(indexPath, JSON.stringify(cache, null, 2));
  }

  private async parseDownloadedSkill(
    skillPath: string,
    skillDir: string,
  ): Promise<SkillConfig> {
    const skillMdPath = path.join(skillDir, 'SKILL.md');
    const content = await fs.readFile(skillMdPath, 'utf8');
    return this.parseSkillMdContent(content, skillMdPath, skillPath);
  }

  private parseSkillMdContent(
    content: string,
    filePath: string,
    remotePath: string,
  ): SkillConfig {
    const config = parseSkillContent(content, filePath);
    return {
      ...config,
      remotePath,
      isRemote: true,
      level: 'extension', // Remote skills use extension level
    };
  }

  /**
   * 使用 Node.js 内置 zlib 解压 zip 文件，适用于标准 zip 文件的实现。
   */
  private async extractZip(zipPath: string, destDir: string): Promise<void> {
    // Use dynamic import for extract-zip as it's an optional dependency
    try {
      const extractZip = await import('extract-zip');
      await extractZip.default(zipPath, { dir: destDir });
    } catch {
      // If extract-zip is not available, try using unzip command
      const { exec } = await import('child_process');
      const { promisify } = await import('util');
      const execAsync = promisify(exec);

      try {
        await execAsync(`unzip -o "${zipPath}" -d "${destDir}"`);
      } catch (unzipError) {
        throw new Error(
          `Failed to extract zip: ${unzipError instanceof Error ? unzipError.message : 'Unknown error'}`,
        );
      }
    }
  }
}

/**
 * 将 RemoteSkillEntry 转换为用于列表显示的轻量级 SkillConfig。
 * 创建一个不包含完整 body 内容的 SkillConfig。
 */
export function remoteEntryToSkillConfig(entry: RemoteSkillEntry): SkillConfig {
  const osMetadata: SkillOSMetadata = {
    version: entry.version,
    layer: entry.layer,
    lifecycle: entry.lifecycle,
    tags: entry.tags,
    status: entry.status,
    dependencies: entry.dependencies,
  };

  return {
    name: entry.name,
    description: entry.description,
    level: 'extension',
    filePath: '',
    body: '',
    remotePath: entry.path,
    isRemote: true,
    osMetadata,
  };
}
