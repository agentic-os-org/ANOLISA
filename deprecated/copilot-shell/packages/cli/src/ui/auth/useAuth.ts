/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  Config,
  ContentGeneratorConfig,
  ModelProvidersConfig,
  AliyunCredentialsExtended,
} from '@copilot-shell/core';
import {
  AuthEvent,
  AuthType,
  encryptCredential,
  getErrorMessage,
  logAuth,
  saveAliyunCredentials,
} from '@copilot-shell/core';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { LoadedSettings } from '../../config/settings.js';
import { getPersistScopeForModelSelection } from '../../config/modelProvidersScope.js';
import type { OpenAICredentials } from '../components/OpenAIKeyPrompt.js';
import { appEvents, AppEvent } from '../../utils/events.js';
import {
  maybeRunKtunerFirstRunCheck,
  isKtunerAvailable,
  hasPromptedConsent,
  markConsentPrompted,
  hasNotifiedUnavailable,
  markNotifiedUnavailable,
} from '../../utils/ktunerFirstRun.js';
import { AuthState, MessageType } from '../types.js';
import type { HistoryItem } from '../types.js';
import { t } from '../../i18n/index.js';

const RECENT_AUTH_MODELS_LIMIT = 10;

function prependRecentModel(
  existing: string[] | undefined,
  model: string,
): string[] {
  const trimmed = model.trim();
  if (!trimmed) {
    return existing ?? [];
  }
  return [
    trimmed,
    ...(existing ?? []).filter((item) => item.trim() !== trimmed),
  ].slice(0, RECENT_AUTH_MODELS_LIMIT);
}

export const useAuthCommand = (
  settings: LoadedSettings,
  config: Config,
  addItem: (item: Omit<HistoryItem, 'id'>, timestamp: number) => void,
  showBashOptionOnStartup: boolean,
  refreshStatic?: () => void,
) => {
  const unAuthenticated = config.getAuthType() === undefined;

  const [authState, setAuthState] = useState<AuthState>(
    unAuthenticated ? AuthState.Updating : AuthState.Unauthenticated,
  );

  const [authError, setAuthError] = useState<string | null>(null);

  const [isAuthenticating, setIsAuthenticating] = useState(false);
  const [isAuthDialogOpen, setIsAuthDialogOpen] = useState(unAuthenticated);
  const [showBashOptionInAuthDialog, setShowBashOptionInAuthDialog] = useState(
    showBashOptionOnStartup,
  );
  const [pendingAuthType, setPendingAuthType] = useState<AuthType | undefined>(
    undefined,
  );

  // Track first-launch auth so we can redraw the static area exactly once
  // after the very first successful login. Without this, the tips banner is
  // pushed into scrollback when the auth dialog unmounts and new history
  // items are appended below Ink's <Static>.
  const isInitialAuthPending = useRef(unAuthenticated);

  const onAuthError = useCallback(
    (error: string | null) => {
      setAuthError(error);
      if (error) {
        setAuthState(AuthState.Updating);
        setIsAuthDialogOpen(true);
      }
    },
    [setAuthError, setAuthState],
  );

  /**
   * Translate authentication error messages for i18n support.
   * Matches known error patterns and returns translated messages.
   */
  const translateAuthError = useCallback((error: unknown): string => {
    const message = getErrorMessage(error);

    // Try to match the message with known error patterns for i18n
    if (message.includes('Invalid API key')) {
      return t('Invalid API key. Please check your API key and try again.');
    }
    if (message.includes('does not have permission')) {
      return t('API key does not have permission to access this resource.');
    }
    if (message.includes('Rate limit exceeded')) {
      return t('Rate limit exceeded. Please check your quota.');
    }
    if (message.includes('is not available')) {
      // Extract model name from message: Model "xxx" is not available
      const modelMatch = message.match(/Model "([^"]+)" is not available/);
      if (modelMatch) {
        return t(
          'Model "{{model}}" is not available. Please check if the model name is correct.',
          { model: modelMatch[1] },
        );
      }
    }

    // Fallback: use the original message
    return message;
  }, []);

  const handleAuthFailure = useCallback(
    (error: unknown) => {
      const errorMessage = t('Failed to authenticate. Message: {{message}}', {
        message: translateAuthError(error),
      });

      // Log authentication failure
      if (pendingAuthType) {
        const authEvent = new AuthEvent(
          pendingAuthType,
          'manual',
          'error',
          errorMessage,
        );
        logAuth(config, authEvent);
      }

      // For OpenAI auth, keep OpenAIKeyPrompt open to show error
      // by NOT calling onAuthError which would open AuthDialog
      if (pendingAuthType === AuthType.USE_OPENAI) {
        setAuthError(errorMessage);
        addItem(
          {
            type: MessageType.ERROR,
            text: t(
              'Configuration verification failed. Please check your API Key and model settings.',
            ),
          },
          Date.now(),
        );
      } else {
        // For other auth types, use onAuthError which opens AuthDialog
        setIsAuthenticating(false);
        setShowBashOptionInAuthDialog(false);
        onAuthError(errorMessage);
      }
    },
    [translateAuthError, onAuthError, pendingAuthType, config, addItem],
  );

  const handleAuthSuccess = useCallback(
    async (authType: AuthType, credentials?: OpenAICredentials) => {
      try {
        const authTypeScope = getPersistScopeForModelSelection(settings);

        // Persist authType
        settings.setValue(
          authTypeScope,
          'security.auth.selectedType',
          authType,
        );

        const contentGeneratorConfig = config.getContentGeneratorConfig();
        const submittedModel = credentials?.model?.trim();
        // For OpenAI auth, persist the model from the post-refresh content
        // generator config. That is the model validateApiKey() actually checked.
        const resolvedModel =
          authType === AuthType.USE_OPENAI
            ? contentGeneratorConfig?.model
            : contentGeneratorConfig?.model || submittedModel;

        if (resolvedModel) {
          settings.setValue(authTypeScope, 'model.name', resolvedModel);
          if (authType === AuthType.USE_OPENAI) {
            settings.setValue(
              authTypeScope,
              'security.auth.openaiModel',
              resolvedModel,
            );
            settings.setValue(
              authTypeScope,
              'security.auth.openaiModels',
              prependRecentModel(
                settings.merged.security?.auth?.openaiModels,
                resolvedModel,
              ),
            );
          } else if (authType === AuthType.USE_ALIYUN) {
            settings.setValue(
              authTypeScope,
              'security.auth.aliyunModel',
              resolvedModel,
            );
            settings.setValue(
              authTypeScope,
              'security.auth.aliyunModels',
              prependRecentModel(
                settings.merged.security?.auth?.aliyunModels,
                resolvedModel,
              ),
            );
          }
        }

        if (credentials) {
          if (credentials?.apiKey != null) {
            settings.setValue(
              authTypeScope,
              'security.auth.apiKey',
              encryptCredential(credentials.apiKey),
            );
          }
          if (credentials?.baseUrl != null) {
            settings.setValue(
              authTypeScope,
              'security.auth.baseUrl',
              credentials.baseUrl,
            );
          }
        }
      } catch (error) {
        handleAuthFailure(error);
        return;
      }

      setAuthError(null);
      setAuthState(AuthState.Authenticated);
      setPendingAuthType(undefined);
      setIsAuthDialogOpen(false);
      setIsAuthenticating(false);

      // Log authentication success
      const authEvent = new AuthEvent(authType, 'manual', 'success');
      logAuth(config, authEvent);

      // Show success message with resolved model name
      const contentGeneratorConfig = config.getContentGeneratorConfig();
      const savedModel =
        authType === AuthType.USE_OPENAI
          ? contentGeneratorConfig?.model
          : contentGeneratorConfig?.model || credentials?.model?.trim();
      addItem(
        {
          type: MessageType.INFO,
          text: savedModel
            ? t(
                '{{authType}} configuration saved successfully, current model: {{model}}',
                { authType, model: savedModel },
              )
            : t('{{authType}} credentials saved successfully.', { authType }),
        },
        Date.now(),
      );

      if (isInitialAuthPending.current) {
        isInitialAuthPending.current = false;
        refreshStatic?.();
      }

      if (process.platform === 'linux') {
        const ktunerMode =
          (settings.merged.general?.ktunerCheck as string | undefined) ?? 'ask';
        if (ktunerMode === 'enabled') {
          if (isKtunerAvailable()) {
            void maybeRunKtunerFirstRunCheck(addItem);
          } else if (!hasNotifiedUnavailable()) {
            markNotifiedUnavailable();
            addItem(
              {
                type: MessageType.INFO,
                text: t(
                  'ktuner check is enabled, but no trusted ktuner binary was found on a system path. See the ktuner install docs.',
                ),
              },
              Date.now(),
            );
          }
        } else if (
          ktunerMode === 'ask' &&
          !hasPromptedConsent() &&
          isKtunerAvailable()
        ) {
          markConsentPrompted();
          addItem(
            {
              type: MessageType.INFO,
              text: t(
                'ktuner detected — read-only kernel tuning suggestions are available. Run /ktuner enable to view them, or /ktuner disable to stop asking.',
              ),
            },
            Date.now(),
          );
        }
      }
    },
    [settings, handleAuthFailure, config, addItem, refreshStatic],
  );

  const performAuth = useCallback(
    async (authType: AuthType, credentials?: OpenAICredentials) => {
      try {
        // Refresh authentication (creates ContentGenerator)
        await config.refreshAuth(authType);

        // Validate API key for OpenAI auth by making a lightweight API call
        // This validates both new credentials and existing saved credentials
        if (authType === AuthType.USE_OPENAI) {
          const contentGenerator = config.getContentGenerator();
          // Check if validateApiKey method exists (it's optional on ContentGenerator interface)
          // LoggingContentGenerator wraps OpenAIContentGenerator, so we check the method existence
          if (
            contentGenerator &&
            typeof contentGenerator.validateApiKey === 'function'
          ) {
            await contentGenerator.validateApiKey();
          }
        }

        // If we get here, authentication and validation succeeded
        handleAuthSuccess(authType, credentials);
      } catch (e) {
        handleAuthFailure(e);
      }
    },
    [config, handleAuthSuccess, handleAuthFailure],
  );

  const isProviderManagedModel = useCallback(
    (authType: AuthType, modelId: string | undefined) => {
      if (!modelId) {
        return false;
      }

      const modelProviders = settings.merged.modelProviders as
        | ModelProvidersConfig
        | undefined;
      if (!modelProviders) {
        return false;
      }
      const providerModels = modelProviders[authType];
      if (!Array.isArray(providerModels)) {
        return false;
      }
      return providerModels.some(
        (providerModel) => providerModel.id === modelId,
      );
    },
    [settings],
  );

  const handleAuthSelect = useCallback(
    async (
      authType: AuthType | undefined,
      credentials?: OpenAICredentials | AliyunCredentialsExtended,
    ) => {
      if (!authType) {
        setShowBashOptionInAuthDialog(false);
        setIsAuthDialogOpen(false);
        setAuthError(null);
        return;
      }

      if (
        authType === AuthType.USE_OPENAI &&
        credentials &&
        'model' in credentials &&
        credentials.model &&
        isProviderManagedModel(authType, credentials.model)
      ) {
        onAuthError(
          t(
            'Model "{{modelName}}" is managed via settings.modelProviders. Please complete the fields in settings, or use another model id.',
            { modelName: credentials.model },
          ),
        );
        return;
      }

      setPendingAuthType(authType);
      setAuthError(null);
      setShowBashOptionInAuthDialog(false);
      setIsAuthDialogOpen(false);
      setIsAuthenticating(true);

      if (authType === AuthType.USE_OPENAI) {
        // Only perform authentication when credentials are provided (from OpenAIKeyPrompt)
        // When credentials are undefined, DialogManager will show OpenAIKeyPrompt for user input
        if (credentials && 'apiKey' in credentials) {
          addItem(
            {
              type: MessageType.INFO,
              text: t('Verifying and saving configuration...'),
            },
            Date.now(),
          );
          // Pass settings.model.generationConfig to updateCredentials so it can be merged
          // after clearing provider-sourced config. This ensures settings.json generationConfig
          // fields (e.g., samplingParams, timeout) are preserved.
          const settingsGenerationConfig = settings.merged.model
            ?.generationConfig as Partial<ContentGeneratorConfig> | undefined;
          config.updateCredentials(
            {
              apiKey: credentials.apiKey,
              baseUrl: credentials.baseUrl,
              model: credentials.model,
            },
            settingsGenerationConfig,
          );
          await performAuth(authType, credentials as OpenAICredentials);
        }
        // If no credentials, just set pendingAuthType and isAuthenticating state
        // DialogManager will show OpenAIKeyPrompt for user to input credentials
        return;
      }

      if (authType === AuthType.USE_ALIYUN) {
        if (credentials && 'accessKeyId' in credentials) {
          try {
            // First validate the credentials structure
            if (
              !credentials.accessKeyId?.trim() ||
              !credentials.accessKeySecret?.trim()
            ) {
              throw new Error('Access Key ID and Secret cannot be empty');
            }

            // Save credentials to ~/.copilot-shell/aliyun_creds.json
            // 支持 STS 凭证（ECS RAM Role）和普通 AK/SK 凭证
            if (credentials.securityToken && credentials.expiration) {
              await saveAliyunCredentials({
                accessKeyId: credentials.accessKeyId.trim(),
                accessKeySecret: credentials.accessKeySecret.trim(),
                securityToken: credentials.securityToken,
                expiration: credentials.expiration,
              });
            } else {
              await saveAliyunCredentials({
                accessKeyId: credentials.accessKeyId.trim(),
                accessKeySecret: credentials.accessKeySecret.trim(),
              });
            }

            // Mark this as manual credentials so syncAfterAuthRefresh preserves the model
            // This is crucial for Aliyun auth to work properly
            const modelsConfig = config.getModelsConfig();
            if (modelsConfig) {
              if (typeof modelsConfig.updateCredentials === 'function') {
                modelsConfig.updateCredentials({
                  model: credentials.model?.trim(),
                });
              }
            }

            // Save model to settings.json for Header display
            if (credentials.model?.trim()) {
              const authTypeScope = getPersistScopeForModelSelection(settings);
              settings.setValue(
                authTypeScope,
                'model.name',
                credentials.model.trim(),
              );
              // 同时写入 aliyun 专属模型字段，避免不同认证方式互相覆盖
              settings.setValue(
                authTypeScope,
                'security.auth.aliyunModel',
                credentials.model.trim(),
              );
              settings.setValue(
                authTypeScope,
                'security.auth.aliyunModels',
                prependRecentModel(
                  settings.merged.security?.auth?.aliyunModels,
                  credentials.model.trim(),
                ),
              );
            }

            // Proceed with authentication
            await performAuth(authType);
          } catch (error) {
            // Ensure we show the error to the user and reset dialog state
            setIsAuthenticating(false);
            setIsAuthDialogOpen(true);
            handleAuthFailure(error);
          }
        }
        return;
      }

      await performAuth(authType);
    },
    [
      config,
      performAuth,
      handleAuthFailure,
      isProviderManagedModel,
      onAuthError,
      settings,
      addItem,
    ],
  );

  const openAuthDialog = useCallback(() => {
    setShowBashOptionInAuthDialog(showBashOptionOnStartup);
    setIsAuthDialogOpen(true);
  }, [showBashOptionOnStartup]);

  const handleContinueToBash = useCallback(() => {
    isInitialAuthPending.current = false;
    setAuthError(null);
    setIsAuthenticating(false);
    setShowBashOptionInAuthDialog(false);
    setIsAuthDialogOpen(false);
    appEvents.emit(
      AppEvent.SpawnShell,
      process.platform === 'win32' ? 'cmd.exe' : 'bash',
    );
  }, []);

  const cancelAuthentication = useCallback(() => {
    // Log authentication cancellation
    if (isAuthenticating && pendingAuthType) {
      const authEvent = new AuthEvent(pendingAuthType, 'manual', 'cancelled');
      logAuth(config, authEvent);
    }

    // Do not reset pendingAuthType here, persist the previously selected type.
    setIsAuthenticating(false);
    setShowBashOptionInAuthDialog(showBashOptionOnStartup);
    setIsAuthDialogOpen(true);
    setAuthError(null);
  }, [isAuthenticating, pendingAuthType, config, showBashOptionOnStartup]);

  /**
   /**
    * We previously used a useEffect to trigger authentication automatically when
    * settings.security.auth.selectedType changed. This caused problems: if authentication failed,
    * the UI could get stuck, since settings.json would update before success. Now, we
    * update selectedType in settings only when authentication fully succeeds.
    * Authentication is triggered explicitly—either during initial app startup or when the
    * user switches methods—not reactively through settings changes. This avoids repeated
    * or broken authentication cycles.
    */
  useEffect(() => {
    const defaultAuthType = process.env['QWEN_DEFAULT_AUTH_TYPE'];
    if (
      defaultAuthType &&
      ![
        AuthType.USE_OPENAI,
        AuthType.USE_ANTHROPIC,
        AuthType.USE_GEMINI,
        AuthType.USE_VERTEX_AI,
      ].includes(defaultAuthType as AuthType)
    ) {
      onAuthError(
        t(
          'Invalid QWEN_DEFAULT_AUTH_TYPE value: "{{value}}". Valid values are: {{validValues}}',
          {
            value: defaultAuthType,
            validValues: [
              AuthType.USE_OPENAI,
              AuthType.USE_ANTHROPIC,
              AuthType.USE_GEMINI,
              AuthType.USE_VERTEX_AI,
            ].join(', '),
          },
        ),
      );
    }
  }, [onAuthError]);

  return {
    authState,
    setAuthState,
    authError,
    onAuthError,
    isAuthDialogOpen,
    showBashOptionInAuthDialog,
    isAuthenticating,
    pendingAuthType,
    handleAuthSelect,
    handleContinueToBash,
    openAuthDialog,
    cancelAuthentication,
  };
};
