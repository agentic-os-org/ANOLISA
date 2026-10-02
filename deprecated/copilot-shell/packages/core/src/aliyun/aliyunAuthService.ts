/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import { exec } from 'node:child_process';
import { promisify } from 'node:util';

const execAsync = promisify(exec);

// ECS 元数据服务地址
const ECS_METADATA_ENDPOINT = 'http://100.100.100.200';

// 管控链接模板
const ALINUX_CONSOLE_URL_TEMPLATE =
  'http://alinux.console.aliyun.com/{regionId}/guide/cosh?instance={instanceId}';

// RAM Role 名称
export const ECS_RAM_ROLE_NAME = 'AliyunECSInstanceForSysomRole';

// 轮询间隔（毫秒）
const POLL_INTERVAL_MS = 2000;

// 最大轮询次数
const MAX_POLL_COUNT = 100;

/**
 * ECS 实例信息
 */
export interface ECSInstanceInfo {
  instanceId: string | null;
  regionId: string | null;
}

/**
 * STS 凭证
 */
export interface STSCredentials {
  accessKeyId: string;
  accessKeySecret: string;
  securityToken: string;
  expiration: string;
}

/**
 * 获取 ECS 实例 ID
 * curl -s http://100.100.100.200/latest/meta-data/instance-id
 * 使用短超时时间，快速检测是否在 ECS 上
 */
export async function getECSInstanceId(): Promise<string | null> {
  try {
    const { stdout } = await execAsync(
      `curl -s --connect-timeout 0.5 --max-time 1 ${ECS_METADATA_ENDPOINT}/latest/meta-data/instance-id`,
    );
    const instanceId = stdout.trim();
    return instanceId && instanceId.length > 0 ? instanceId : null;
  } catch {
    return null;
  }
}

/**
 * 获取 ECS Region ID
 * 注意：元数据服务不直接提供 region-id，通过 zone-id 截取末尾字母推导
 * 例：cn-hangzhou-j → cn-hangzhou
 */
export async function getECSRegionId(): Promise<string | null> {
  try {
    const { stdout } = await execAsync(
      `curl -s --connect-timeout 1 --max-time 2 ${ECS_METADATA_ENDPOINT}/latest/meta-data/zone-id`,
    );
    const zoneId = stdout.trim();
    if (!zoneId || zoneId.length === 0) return null;
    // 去掉末尾的 -j、-b 等可用区后缀，推导出 region-id
    const regionId = zoneId.replace(/-[a-z]$/, '');
    return regionId.length > 0 ? regionId : null;
  } catch {
    return null;
  }
}

/**
 * 获取完整的 ECS 实例信息
 */
export async function getECSInstanceInfo(): Promise<ECSInstanceInfo> {
  const [instanceId, regionId] = await Promise.all([
    getECSInstanceId(),
    getECSRegionId(),
  ]);
  return { instanceId, regionId };
}

/**
 * 生成管控链接
 */
export function generateConsoleUrl(
  instanceId: string,
  regionId?: string | null,
): string {
  return ALINUX_CONSOLE_URL_TEMPLATE.replace(
    '{regionId}',
    regionId ?? '',
  ).replace('{instanceId}', instanceId);
}

/**
 * 检查当前 ECS 实例是否被授予了指定的 RAM Role
 * curl http://100.100.100.200/latest/meta-data/ram/security-credentials/{role-name}
 */
export async function checkECSRamRoleAuthorized(
  roleName: string = ECS_RAM_ROLE_NAME,
): Promise<boolean> {
  try {
    const { stdout, stderr } = await execAsync(
      `curl -s --connect-timeout 2 --max-time 3 ${ECS_METADATA_ENDPOINT}/latest/meta-data/ram/security-credentials/${roleName}`,
    );
    if (stderr) {
      return false;
    }
    const response = stdout.trim();
    // 如果返回包含 AccessKeyId，说明角色已授权
    return response.includes('AccessKeyId');
  } catch {
    return false;
  }
}

/**
 * 获取 ECS RAM Role 的 STS 临时凭证
 */
export async function getECSRamRoleCredentials(
  roleName: string = ECS_RAM_ROLE_NAME,
): Promise<STSCredentials | null> {
  try {
    const { stdout, stderr } = await execAsync(
      `curl -s --connect-timeout 2 --max-time 5 ${ECS_METADATA_ENDPOINT}/latest/meta-data/ram/security-credentials/${roleName}`,
    );
    if (stderr) {
      return null;
    }
    const response = JSON.parse(stdout.trim());

    // 验证响应格式
    if (
      response.AccessKeyId &&
      response.AccessKeySecret &&
      response.SecurityToken
    ) {
      return {
        accessKeyId: response.AccessKeyId,
        accessKeySecret: response.AccessKeySecret,
        securityToken: response.SecurityToken,
        expiration: response.Expiration,
      };
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * 轮询等待 ECS RAM Role 授权
 * @returns 是否成功获取授权
 */
export async function pollForECSRamRoleAuthorization(
  roleName: string = ECS_RAM_ROLE_NAME,
  onPoll?: (attempt: number) => void,
): Promise<boolean> {
  for (let i = 0; i < MAX_POLL_COUNT; i++) {
    if (onPoll) {
      onPoll(i + 1);
    }

    const isAuthorized = await checkECSRamRoleAuthorized(roleName);
    if (isAuthorized) {
      return true;
    }

    // 等待下一次轮询
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }
  return false;
}

/**
 * 检查 STS 凭证是否已过期
 */
export function isSTSCredentialsExpired(expiration: string): boolean {
  const expirationDate = new Date(expiration);
  return Date.now() >= expirationDate.getTime();
}

/**
 * 获取最新的 STS 凭证（直接调用 ECS RAM Role API，不缓存）
 */
export async function getValidSTSCredentials(
  roleName: string = ECS_RAM_ROLE_NAME,
): Promise<STSCredentials | null> {
  return getECSRamRoleCredentials(roleName);
}

/**
 * 判断当前环境是否在阿里云 ECS 上
 */
export async function isRunningOnECS(): Promise<boolean> {
  const instanceId = await getECSInstanceId();
  return instanceId !== null;
}

/**
 * Aliyun Authentication方式
 */
export enum AliyunAuthMethod {
  WEB_AUTH = 'web_auth', // 网页认证（ECS 管控）
  ECS_RAM_ROLE = 'ecs_ram_role', // ECS RAM Role 自动认证
  AK_SK = 'ak_sk', // AK/SK 手动输入
}

/**
 * Aliyun Authentication结果
 */
export interface AliyunAuthResult {
  method: AliyunAuthMethod;
  credentials?: STSCredentials;
  instanceId?: string;
  consoleUrl?: string;
  success: boolean;
  error?: string;
}
