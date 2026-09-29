/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { Box, Text } from 'ink';
import Link from 'ink-link';
import chalk from 'chalk';
import qrcode from 'qrcode-terminal';
import { Colors } from '../colors.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';
import { z } from 'zod';
import {
  ALIYUN_DEFAULT_MODEL,
  type STSCredentials,
  type AliyunCredentials,
  AliyunAuthMethod,
  ECS_RAM_ROLE_NAME,
  getECSInstanceId,
  getECSRegionId,
  generateConsoleUrl,
  pollForECSRamRoleAuthorization,
  getECSRamRoleCredentials,
} from '@copilot-shell/core';

// AK/SK 表单校验 schema
const aliyunCredentialSchema = z.object({
  accessKeyId: z.string().min(1, 'Access Key ID is required'),
  accessKeySecret: z.string().min(1, 'Access Key Secret is required'),
  model: z.string().min(1, 'Model must be a non-empty string').optional(),
});

/**
 * 静态显示区域：URL
 * 只在 consoleUrl 变化时才重新渲染，避免轮询动画导致闪烁
 */
function EcsAuthStaticDisplay({
  consoleUrl,
  instanceId,
  qrCodeData,
}: {
  consoleUrl: string | null;
  instanceId: string | null;
  qrCodeData: string | null;
}): React.JSX.Element {
  return (
    <Box
      borderStyle="round"
      borderColor={Colors.AccentBlue}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold color={Colors.AccentBlue}>
        {t('Aliyun Authentication')}
      </Text>
      <Box marginTop={1}>
        <Text>
          {t(
            'Please click or copy the URL below to your browser to complete authentication:',
          )}
        </Text>
      </Box>
      {consoleUrl && (
        <Box marginTop={1} flexDirection="column">
          <Link url={consoleUrl} fallback={false}>
            <Text color={Colors.AccentBlue}>{consoleUrl}</Text>
          </Link>
        </Box>
      )}
      {instanceId && (
        <Box marginTop={1}>
          <Text>
            {t('ECS Instance ID:')} {instanceId}
          </Text>
        </Box>
      )}
      {qrCodeData && (
        <Box marginTop={1} flexDirection="column">
          <Text>{t('Or scan the QR code below:')}</Text>
          <Box marginTop={1}>
            <Text>{qrCodeData}</Text>
          </Box>
        </Box>
      )}
    </Box>
  );
}

/**
 * 动态状态行：轮询状态展示（无动画，避免 state 变化导致镇屏闪烁）
 */
function EcsPollingStatus({ step }: { step: string }): React.JSX.Element {
  return (
    <Box
      borderStyle="round"
      borderColor={Colors.AccentBlue}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Box marginTop={1}>
        <Text>
          {'\u280b'}{' '}
          {step === 'polling_role'
            ? t('Waiting for authorization')
            : t('Preparing authentication...')}
        </Text>
      </Box>
      <Box marginTop={1} justifyContent="space-between">
        <Text color={Colors.Gray}>{t('(Press Esc to cancel)')}</Text>
      </Box>
    </Box>
  );
}

interface AliyunAuthPromptProps {
  isAuthenticating: boolean;
  onSubmit: (
    method: AliyunAuthMethod,
    credentials: STSCredentials | AliyunCredentials,
    model: string,
  ) => void;
  onCancel: () => void;
  defaultModel?: string;
}

type AuthStep =
  | 'detecting' // 检测环境中
  | 'web_auth' // 网页认证（展示链接/二维码）
  | 'polling_role' // 轮询等待 RAM Role 授权
  | 'aksk_input' // AK/SK 输入
  | 'success' // 认证成功
  | 'error'; // 错误

interface AuthState {
  step: AuthStep;
  isOnECS: boolean;
  instanceId: string | null;
  consoleUrl: string | null;
  errorMessage: string | null;
  stsCredentials: STSCredentials | null;
}

export function AliyunAuthPrompt({
  isAuthenticating,
  onSubmit,
  onCancel,
  defaultModel,
}: AliyunAuthPromptProps): React.JSX.Element | null {
  const [state, setState] = useState<AuthState>({
    step: 'detecting',
    isOnECS: false,
    instanceId: null,
    consoleUrl: null,
    errorMessage: null,
    stsCredentials: null,
  });

  // 加载动画的点数状态 (0-3)
  // const [loadingDots, setLoadingDots] = useState(3);

  // 二维码数据
  const [qrCodeData, setQrCodeData] = useState<string | null>(null);

  // 加载动画效果（已移至独立的 EcsPollingStatus 子组件，避免更新泥染父组件）
  // useEffect(() => {
  //   if (state.step !== 'polling_role') { return undefined; }
  //   const interval = setInterval(() => { setLoadingDots((prev) => (prev + 1) % 4); }, 500);
  //   return () => clearInterval(interval);
  // }, [state.step]);

  // 生成二维码
  useEffect(() => {
    if (state.consoleUrl && state.step === 'polling_role') {
      try {
        qrcode.generate(state.consoleUrl, { small: true }, (qrcode: string) => {
          setQrCodeData(qrcode);
        });
      } catch (error) {
        console.error('Failed to generate QR code:', error);
        setQrCodeData(null);
      }
    }
  }, [state.consoleUrl, state.step]);

  const [model, setModel] = useState(defaultModel || ALIYUN_DEFAULT_MODEL);
  // 使用 ref 存储 model，避免 useCallback 依赖变化
  const modelRef = useRef(model);
  useEffect(() => {
    modelRef.current = model;
  }, [model]);

  // AK/SK 输入状态
  const [accessKeyId, setAccessKeyId] = useState('');
  const [accessKeySecret, setAccessKeySecret] = useState('');
  const [currentField, setCurrentField] = useState<
    'accessKeyId' | 'accessKeySecret' | 'model'
  >('accessKeyId');

  // AK/SK 始终显示为空，需要用户重新输入
  // 不加载已保存的凭证，保持安全

  // 用于防止组件卸载后回调泄漏
  const isMountedRef = useRef(true);
  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  // 用于防止重复提交的 ref
  const hasSubmittedRef = useRef(false);

  // 开始轮询检测 RAM Role
  const startPollingForRole = useCallback(async () => {
    // 轮询已经在进行中，状态已经是 'polling_role'

    const isAuthorized =
      await pollForECSRamRoleAuthorization(ECS_RAM_ROLE_NAME);

    // 如果组件已卸载，不进行任何状态更新
    if (!isMountedRef.current) return;

    if (isAuthorized) {
      // 获取 STS 凭证
      const credentials = await getECSRamRoleCredentials(ECS_RAM_ROLE_NAME);

      if (!isMountedRef.current) return;

      if (credentials) {
        setState((prev) => ({
          ...prev,
          step: 'success',
          stsCredentials: credentials,
        }));
        // 立即提交
        if (!hasSubmittedRef.current) {
          hasSubmittedRef.current = true;
          onSubmit(
            AliyunAuthMethod.ECS_RAM_ROLE,
            credentials,
            modelRef.current,
          );
        }
      } else {
        setState((prev) => ({
          ...prev,
          step: 'error',
          errorMessage: t('Failed to get STS credentials'),
        }));
      }
    } else {
      setState((prev) => ({
        ...prev,
        step: 'error',
        errorMessage: t('Timeout waiting for RAM role authorization'),
      }));
    }
  }, [onSubmit]);

  // 初始化：检测环境
  useEffect(() => {
    const detectEnvironment = async () => {
      // 先探测是否在 ECS 上（短超时快速判断）
      const instanceId = await getECSInstanceId();
      if (!instanceId) {
        // 不在 ECS 上，进入 AK/SK 输入页面
        setState((prev) => ({ ...prev, step: 'aksk_input', isOnECS: false }));
        return;
      }

      // 确认在 ECS 上，再获取 regionId 并生成 URL
      const regionId = await getECSRegionId();
      const url = generateConsoleUrl(instanceId, regionId);
      setState((prev) => ({
        ...prev,
        step: 'polling_role',
        isOnECS: true,
        instanceId,
        consoleUrl: url,
      }));

      // 开始轮询检测 RAM Role
      startPollingForRole();
    };

    detectEnvironment();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // AK/SK 验证和提交
  const validateAndSubmitAKSK = useCallback(() => {
    try {
      const validated = aliyunCredentialSchema.parse({
        accessKeyId: accessKeyId.trim(),
        accessKeySecret: accessKeySecret.trim(),
        model: model.trim() || undefined,
      });

      onSubmit(
        AliyunAuthMethod.AK_SK,
        {
          accessKeyId: validated.accessKeyId,
          accessKeySecret: validated.accessKeySecret,
        },
        validated.model || ALIYUN_DEFAULT_MODEL,
      );
    } catch (error) {
      // Zod validation error - show to user
      const message =
        error instanceof Error ? error.message : t('Invalid credentials');
      setState((prev) => ({
        ...prev,
        step: 'error',
        errorMessage: message,
      }));
    }
  }, [accessKeyId, accessKeySecret, model, onSubmit]);

  // 键盘处理
  useKeypress(
    (key) => {
      // ESC 或 Ctrl+C 取消
      if (key.name === 'escape' || (key.ctrl && key.name === 'c')) {
        onCancel();
        return;
      }

      // AK/SK 输入时的键盘处理
      if (state.step === 'aksk_input') {
        if (key.name === 'return') {
          if (currentField === 'accessKeyId') {
            setCurrentField('accessKeySecret');
          } else if (currentField === 'accessKeySecret') {
            setCurrentField('model');
          } else if (currentField === 'model') {
            if (!accessKeyId.trim()) {
              // AK 为空，跳转到 AK 字段
              setCurrentField('accessKeyId');
            } else if (!accessKeySecret.trim()) {
              // SK 为空，跳转到 SK 字段
              setCurrentField('accessKeySecret');
            } else {
              // AK 和 SK 都有值，提交
              validateAndSubmitAKSK();
            }
          }
        } else if (key.name === 'tab') {
          if (currentField === 'accessKeyId') {
            setCurrentField('accessKeySecret');
          } else if (currentField === 'accessKeySecret') {
            setCurrentField('model');
          } else {
            setCurrentField('accessKeyId');
          }
        } else if (key.name === 'up') {
          if (currentField === 'accessKeySecret') {
            setCurrentField('accessKeyId');
          } else if (currentField === 'model') {
            setCurrentField('accessKeySecret');
          }
        } else if (key.name === 'down') {
          if (currentField === 'accessKeyId') {
            setCurrentField('accessKeySecret');
          } else if (currentField === 'accessKeySecret') {
            setCurrentField('model');
          }
        } else if (key.name === 'backspace' || key.name === 'delete') {
          if (currentField === 'accessKeyId') {
            setAccessKeyId((prev) => prev.slice(0, -1));
          } else if (currentField === 'accessKeySecret') {
            setAccessKeySecret((prev) => prev.slice(0, -1));
          } else if (currentField === 'model') {
            setModel((prev) => prev.slice(0, -1));
          }
        } else if (key.sequence && !key.ctrl && !key.meta) {
          const cleanInput = key.sequence
            .split('')
            .filter((ch) => ch.charCodeAt(0) >= 32)
            .join('');

          if (cleanInput.length > 0) {
            if (currentField === 'accessKeyId') {
              setAccessKeyId((prev) => prev + cleanInput);
            } else if (currentField === 'accessKeySecret') {
              setAccessKeySecret((prev) => prev + cleanInput);
            } else if (currentField === 'model') {
              setModel((prev) => prev + cleanInput);
            }
          }
        }
        return;
      }

      // 错误状态时按任意键返回
      if (state.step === 'error' && key.name === 'return') {
        // 根据是否在 ECS 上决定回退到哪个步骤
        if (state.isOnECS) {
          // ECS 环境下错误，没有可回退的页面，取消认证
          onCancel();
        } else {
          setState((prev) => ({
            ...prev,
            step: 'aksk_input',
            errorMessage: null,
          }));
        }
      }
    },
    { isActive: true },
  );

  const ecsStaticDisplay = useMemo(
    () => (
      <EcsAuthStaticDisplay
        consoleUrl={state.consoleUrl}
        instanceId={state.instanceId}
        qrCodeData={qrCodeData}
      />
    ),
    [state.consoleUrl, state.instanceId, qrCodeData],
  );

  // 渲染检测中状态
  if (state.step === 'detecting') {
    return (
      <Box
        borderStyle="round"
        borderColor={Colors.AccentBlue}
        flexDirection="column"
        padding={1}
        width="100%"
      >
        <Text bold color={Colors.AccentBlue}>
          {t('Aliyun Authentication')}
        </Text>
        <Box marginTop={1}>
          <Text>{t('Detecting environment...')}</Text>
        </Box>
      </Box>
    );
  }

  // 渲染网页认证（展示链接）
  if (state.step === 'web_auth' || state.step === 'polling_role') {
    return (
      <Box flexDirection="column" width="100%">
        {/* 静态区域：URL + 二维码，useMemo 记忆化避免重新渲染 */}
        {ecsStaticDisplay}
        {/* 动态状态行：独立子组件，更新不影响静态区域 */}
        <EcsPollingStatus step={state.step} />
      </Box>
    );
  }

  // 渲染 AK/SK 输入
  if (state.step === 'aksk_input') {
    const maskedAccessKeyId = accessKeyId
      ? accessKeyId.slice(0, Math.min(3, accessKeyId.length)) +
        '*'.repeat(Math.max(0, accessKeyId.length - 3))
      : '';
    const maskedSecret = accessKeySecret
      ? '*'.repeat(accessKeySecret.length)
      : '';

    return (
      <Box
        borderStyle="round"
        borderColor={Colors.AccentBlue}
        flexDirection="column"
        padding={1}
        width="100%"
      >
        <Text bold color={Colors.AccentBlue}>
          {t('Aliyun Authentication')}
        </Text>
        <Box marginTop={1}>
          <Text>
            {t(
              'Please enter your Aliyun Access Key credentials. You can get them from',
            )}{' '}
            <Text color={Colors.AccentBlue}>
              https://ram.console.aliyun.com/manage/ak
            </Text>
          </Text>
        </Box>
        <Box marginTop={1} flexDirection="row">
          <Box width={20}>
            <Text
              color={
                currentField === 'accessKeyId' ? Colors.AccentBlue : Colors.Gray
              }
            >
              {t('Access Key ID:')}
            </Text>
          </Box>
          <Box flexGrow={1}>
            <Text>
              {currentField === 'accessKeyId' ? '> ' : '  '}
              {currentField === 'accessKeyId'
                ? `${accessKeyId}${chalk.inverse(' ')}`
                : maskedAccessKeyId || ' '}
            </Text>
          </Box>
        </Box>
        <Box marginTop={1} flexDirection="row">
          <Box width={20}>
            <Text
              color={
                currentField === 'accessKeySecret'
                  ? Colors.AccentBlue
                  : Colors.Gray
              }
            >
              {t('Access Key Secret:')}
            </Text>
          </Box>
          <Box flexGrow={1}>
            <Text>
              {currentField === 'accessKeySecret' ? '> ' : '  '}
              {currentField === 'accessKeySecret'
                ? `${maskedSecret}${chalk.inverse(' ')}`
                : maskedSecret || ' '}
            </Text>
          </Box>
        </Box>
        <Box marginTop={1} flexDirection="row">
          <Box width={20}>
            <Text
              color={currentField === 'model' ? Colors.AccentBlue : Colors.Gray}
            >
              {t('Model:')}
            </Text>
          </Box>
          <Box flexGrow={1}>
            <Text>
              {currentField === 'model' ? '> ' : '  '}
              {currentField === 'model'
                ? `${model}${chalk.inverse(' ')}`
                : model || ' '}
            </Text>
          </Box>
        </Box>
        <Box marginTop={1}>
          <Text color={Colors.Gray}>
            {t('Press Enter to continue, Tab/↑↓ to navigate, Esc to cancel')}
          </Text>
        </Box>
      </Box>
    );
  }

  // 渲染错误状态
  if (state.step === 'error') {
    return (
      <Box
        borderStyle="round"
        borderColor={Colors.AccentRed}
        flexDirection="column"
        padding={1}
        width="100%"
      >
        <Text bold color={Colors.AccentRed}>
          {t('Aliyun Authentication')}
        </Text>
        <Box marginTop={1}>
          <Text color={Colors.AccentRed}>
            {state.errorMessage || t('Authentication failed')}
          </Text>
        </Box>
        <Box marginTop={1}>
          <Text color={Colors.Gray}>
            {state.isOnECS
              ? t('(Press ESC or CTRL+C to cancel)')
              : t('Press Enter to go back · Esc to cancel')}
          </Text>
        </Box>
      </Box>
    );
  }

  // 成功状态：如果仍在认证中，保持显示轮询状态避免闪烁
  // 如果认证已完成，返回 null
  if (!isAuthenticating) {
    return null;
  }

  // 认证中但状态为 success，显示一个空的占位符避免闪烁
  return (
    <Box width="100%">
      <Text> </Text>
    </Box>
  );
}
