/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import * as path from 'node:path';
import { promises as fs } from 'node:fs';
import {
  encryptCredential,
  decryptCredential,
} from '../utils/credential-encryptor.js';
import { Storage } from '../config/storage.js';
import { loadCoshNgAuth } from '../config/coshNgAuth.js';

const ALIYUN_CREDS_FILENAME = 'aliyun_creds.json';

/**
 * Default model for Aliyun auth
 */
export const ALIYUN_DEFAULT_MODEL = 'qwen3.7-plus';

/**
 * 阿里云凭证类型分层说明
 *
 * ┌─────────────────────────────────────────────────────────────┐
 * │ AliyunCredentials          基础层：仅 AK/SK，最小凭证集    │
 * │   └─ AliyunSTSCredentials  扩展层：追加 STS Token + 过期时间│
 * │   └─ AliyunCredentialsExtended  UI 传递层：携带 model/method│
 * │                                 用于 onSubmit → handleAuth  │
 * │ AliyunCredentialsWithSTS   联合类型：存储/SDK 入参使用      │
 * └─────────────────────────────────────────────────────────────┘
 *
 * 使用边界：
 *   - core 层内部（SDK 调用、磁盘存储）→ AliyunCredentials / AliyunSTSCredentials / AliyunCredentialsWithSTS
 *   - UI → core 回调传参             → AliyunCredentialsExtended
 *   - 运行时 STS 刷新                → AliyunSTSCredentials（来自 ECS RAM Role API）
 */

/**
 * 阿里云 AK/SK 凭证接口
 */
export interface AliyunCredentials {
  accessKeyId: string;
  accessKeySecret: string;
}

/**
 * 阿里云 STS 凭证接口（ECS RAM Role 使用）
 */
export interface AliyunSTSCredentials extends AliyunCredentials {
  securityToken: string;
  expiration: string;
}

/**
 * AK/SK 凭证与 STS 凭证的联合类型
 */
export type AliyunCredentialsWithSTS = AliyunCredentials | AliyunSTSCredentials;

/**
 * 认证流程参数传递类型：在 AliyunCredentials 基础上携带可选 STS 字段、
 * 模型配置和认证方式，用于 UI 层 onSubmit 回调到 handleAuthSelect 的数据传递
 */
export interface AliyunCredentialsExtended extends AliyunCredentials {
  securityToken?: string;
  expiration?: string;
  model?: string;
  method?: string;
}

/**
 * 获取阿里云凭证文件路径
 */
export function getAliyunCredsPath(): string {
  return path.join(Storage.getGlobalQwenDir(), ALIYUN_CREDS_FILENAME);
}

/**
 * 将阿里云凭证加密保存到磁盘。
 * 同时支持 AK/SK 凭证和 STS 凭证。
 */
export async function saveAliyunCredentials(
  credentials: AliyunCredentialsWithSTS,
): Promise<void> {
  const filePath = getAliyunCredsPath();
  try {
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    const encrypted = encryptCredential(JSON.stringify(credentials));
    await fs.writeFile(filePath, encrypted, { mode: 0o600 });
  } catch (error: unknown) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    const errorCode =
      error instanceof Error && 'code' in error
        ? (error as Error & { code?: string }).code
        : undefined;

    if (errorCode === 'EACCES') {
      throw new Error(
        `Failed to save Aliyun credentials: Permission denied (EACCES). Please check permissions for \`${filePath}\`.`,
      );
    }

    throw new Error(
      `Failed to save Aliyun credentials: ${errorMessage}. Please check permissions.`,
    );
  }
}

/**
 * 从磁盘加载阿里云凭证。
 * 支持加密格式（enc: 前缀）和明文 JSON（向前兼容）。
 * 若存在 STS 凭证则返回 STS 类型，否则返回普通 AK/SK 类型。
 *
 * The native `aliyun_creds.json` wins. Only when it is absent is the long-lived
 * AK/SK recorded in cosh-ng's `config.toml` reused read-only (see
 * {@link loadCoshNgAuth}), so switching back with `cosh-switch` does not force
 * re-authentication. A corrupt or unreadable native store keeps the prior
 * re-authentication behavior instead of silently switching identities.
 */
export async function loadAliyunCredentials(): Promise<AliyunCredentialsWithSTS | null> {
  const native = await loadNativeAliyunCredentials();
  if (native.status === 'loaded') {
    return native.credentials;
  }
  if (native.status === 'invalid') {
    return null;
  }
  return loadCoshNgAliyunCredentials();
}

/**
 * Read-only fallback to cosh-ng's long-lived AK/SK.
 *
 * Only ever yields {@link AliyunCredentials}: both the ECS RAM role flow and
 * temporary STS credentials are refused outright by {@link loadCoshNgAuth}.
 * That is required rather than merely cautious - the generator treats any
 * `securityToken` as STS, and on expiry `refreshSTSCredentials()` persists the
 * refreshed pair to `aliyun_creds.json`, at which point the fallback would no
 * longer be read-only.
 */
function loadCoshNgAliyunCredentials(): AliyunCredentials | null {
  const coshNgAuth = loadCoshNgAuth();
  if (coshNgAuth?.kind !== 'aliyun') {
    return null;
  }

  return {
    accessKeyId: coshNgAuth.accessKeyId,
    accessKeySecret: coshNgAuth.accessKeySecret,
  };
}

type NativeAliyunCredentialsResult =
  | { status: 'loaded'; credentials: AliyunCredentialsWithSTS }
  | { status: 'absent' }
  | { status: 'invalid' };

async function loadNativeAliyunCredentials(): Promise<NativeAliyunCredentialsResult> {
  const filePath = getAliyunCredsPath();
  try {
    const content = await fs.readFile(filePath, 'utf-8');
    const decrypted = decryptCredential(content);
    if (decrypted === undefined) {
      // Decryption failed (e.g. salt changed) — treat as corrupted
      console.warn('Failed to decrypt Aliyun credentials file');
      return { status: 'invalid' };
    }

    const credentials = JSON.parse(decrypted) as AliyunCredentialsWithSTS;

    // Validate credentials structure
    if (!credentials.accessKeyId || !credentials.accessKeySecret) {
      console.warn('Invalid Aliyun credentials format in file');
      return { status: 'invalid' };
    }

    return { status: 'loaded', credentials };
  } catch (error: unknown) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') {
      // File doesn't exist
      return { status: 'absent' };
    }
    console.warn('Failed to load Aliyun credentials:', error);
    return { status: 'invalid' };
  }
}

/**
 * 删除磁盘上的阿里云凭证文件
 */
export async function clearAliyunCredentials(): Promise<void> {
  const filePath = getAliyunCredsPath();
  try {
    await fs.unlink(filePath);
    console.debug('Aliyun credentials cleared successfully.');
  } catch (error: unknown) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') {
      // File doesn't exist, already cleared
      return;
    }
    console.warn('Warning: Failed to clear Aliyun credentials:', error);
  }
}

/**
 * 检查阿里云凭证是否已保存
 */
export async function hasAliyunCredentials(): Promise<boolean> {
  const credentials = await loadAliyunCredentials();
  return credentials !== null;
}
