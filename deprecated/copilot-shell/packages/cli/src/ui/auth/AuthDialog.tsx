/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState, useCallback, useEffect, useMemo } from 'react';
import { AuthType } from '@copilot-shell/core';
import { Box, Text } from 'ink';
import Spinner from 'ink-spinner';
import { Colors } from '../colors.js';
import { useKeypress } from '../hooks/useKeypress.js';
import type { Key } from '../hooks/useKeypress.js';
import { DescriptiveRadioButtonSelect } from '../components/shared/DescriptiveRadioButtonSelect.js';
import { useUIState } from '../contexts/UIStateContext.js';
import { useUIActions } from '../contexts/UIActionsContext.js';
import { useConfig } from '../contexts/ConfigContext.js';
import { t } from '../../i18n/index.js';

function parseDefaultAuthType(
  defaultAuthType: string | undefined,
): AuthType | null {
  if (
    defaultAuthType &&
    Object.values(AuthType).includes(defaultAuthType as AuthType)
  ) {
    return defaultAuthType as AuthType;
  }
  return null;
}

export function AuthDialog(): React.JSX.Element {
  const { pendingAuthType, authError, showBashOptionInAuthDialog } =
    useUIState();
  const {
    handleAuthSelect: onAuthSelect,
    handleContinueToBash: onContinueToBash,
  } = useUIActions();
  const config = useConfig();

  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  // While a provider selection is being handed off to the auth flow (which may
  // synchronously probe provider config before the next dialog renders), show a
  // loading indicator and disable input so repeated keys cannot re-trigger it.
  const [isSelectingAuth, setIsSelectingAuth] = useState(false);
  const [pendingSelection, setPendingSelection] = useState<AuthType | null>(
    null,
  );
  const authItems = useMemo(
    () => [
      {
        key: AuthType.USE_ALIYUN,
        label: t('Aliyun Authentication'),
        title: t('Aliyun Authentication'),
        description: t('Free with limited quota'),
        value: AuthType.USE_ALIYUN,
      },
      {
        key: AuthType.USE_OPENAI,
        label: t('Custom Provider'),
        title: t('Custom Provider'),
        description: t(
          'Paid · Use your own API key · Cost depends on provider',
        ),
        value: AuthType.USE_OPENAI,
      },
    ],
    [],
  );
  const bashItem = useMemo(
    () => ({
      key: 'bash',
      label: t('Continue to Bash'),
      title: t('Continue to Bash'),
      description: t(
        'Open an interactive Bash shell without configuring AI authentication',
      ),
      value: 'bash' as const,
    }),
    [],
  );

  const orderedOptions = useMemo(
    () =>
      showBashOptionInAuthDialog
        ? [...authItems.map((item) => item.value), bashItem.value]
        : authItems.map((item) => item.value),
    [authItems, bashItem, showBashOptionInAuthDialog],
  );

  const initialAuthIndex = Math.max(
    0,
    authItems.findIndex((item) => {
      if (pendingAuthType) {
        return item.value === pendingAuthType;
      }

      const currentAuthType = config.getAuthType();
      if (currentAuthType) {
        return item.value === currentAuthType;
      }

      const defaultAuthType = parseDefaultAuthType(
        process.env['QWEN_DEFAULT_AUTH_TYPE'],
      );
      if (defaultAuthType) {
        return item.value === defaultAuthType;
      }

      return item.value === AuthType.USE_ALIYUN;
    }),
  );

  const [activeOption, setActiveOption] = useState<AuthType | 'bash'>(
    authItems[initialAuthIndex]?.value ?? AuthType.USE_ALIYUN,
  );
  const [lastAuthOption, setLastAuthOption] = useState<AuthType>(
    authItems[initialAuthIndex]?.value ?? AuthType.USE_ALIYUN,
  );

  useEffect(() => {
    const nextOption =
      authItems[initialAuthIndex]?.value ?? AuthType.USE_ALIYUN;
    setActiveOption(nextOption);
    setLastAuthOption(nextOption);
  }, [initialAuthIndex, authItems]);

  const activeSection = activeOption === 'bash' ? 'bash' : 'auth';
  const authSelectedIndex = Math.max(
    0,
    authItems.findIndex((item) => item.value === lastAuthOption),
  );

  const authDisplayIndex = activeSection === 'auth' ? authSelectedIndex : null;
  const bashDisplayIndex =
    showBashOptionInAuthDialog && activeSection === 'bash' ? 0 : null;
  const isManualAuthDialog = !showBashOptionInAuthDialog;

  useEffect(() => {
    if (!showBashOptionInAuthDialog && activeOption === 'bash') {
      const nextOption =
        authItems[initialAuthIndex]?.value ??
        lastAuthOption ??
        AuthType.USE_ALIYUN;
      setActiveOption(nextOption);
      setLastAuthOption(nextOption);
    }
  }, [
    showBashOptionInAuthDialog,
    activeOption,
    authItems,
    initialAuthIndex,
    lastAuthOption,
  ]);

  const handleAuthSelect = useCallback((authMethod: AuthType) => {
    setErrorMessage(null);
    // Enter loading state and defer the actual hand-off (see effect below) so the
    // spinner paints before the auth flow's synchronous work blocks rendering.
    setIsSelectingAuth(true);
    setPendingSelection(authMethod);
  }, []);

  // Run the deferred provider selection once the loading indicator has rendered.
  // If the hand-off rejects without unmounting this dialog, restore input so the
  // user is not stuck on the spinner.
  useEffect(() => {
    if (!isSelectingAuth || pendingSelection === null) {
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      void (async () => {
        try {
          await onAuthSelect(pendingSelection);
        } catch {
          if (!cancelled) {
            setIsSelectingAuth(false);
            setPendingSelection(null);
          }
        }
      })();
    }, 0);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [isSelectingAuth, pendingSelection, onAuthSelect]);

  const handleContinueToBash = useCallback(() => {
    setErrorMessage(null);
    onContinueToBash();
  }, [onContinueToBash]);

  const handleHighlight = useCallback((value: AuthType | 'bash') => {
    setActiveOption(value);
    if (value !== 'bash') {
      setLastAuthOption(value);
    }
  }, []);

  const moveOption = useCallback(
    (direction: 'up' | 'down') => {
      const currentIndex = orderedOptions.findIndex(
        (option) => option === activeOption,
      );
      const currentSafeIndex = currentIndex >= 0 ? currentIndex : 0;
      const step = direction === 'down' ? 1 : -1;
      const nextIndex =
        (currentSafeIndex + step + orderedOptions.length) %
        orderedOptions.length;
      const nextOption = orderedOptions[nextIndex] ?? orderedOptions[0];
      handleHighlight(nextOption);
    },
    [orderedOptions, activeOption, handleHighlight],
  );

  const moveSection = useCallback(
    (direction: 'forward' | 'backward') => {
      if (!showBashOptionInAuthDialog) {
        return;
      }

      if (direction === 'forward') {
        if (activeSection === 'auth') {
          handleHighlight('bash');
          return;
        }
        handleHighlight(lastAuthOption);
        return;
      }

      if (activeSection === 'bash') {
        handleHighlight(lastAuthOption);
        return;
      }
      handleHighlight('bash');
    },
    [
      showBashOptionInAuthDialog,
      activeSection,
      lastAuthOption,
      handleHighlight,
    ],
  );

  const handleDialogKeypress = useCallback(
    (key: Key) => {
      if (key.name === 'escape') {
        if (errorMessage) {
          return;
        }
        if (config.getAuthType() === undefined) {
          setErrorMessage(
            t(
              'You must select an auth method or continue to Bash to proceed. Press Ctrl+C again to exit.',
            ),
          );
          return;
        }
        onAuthSelect(undefined);
        return;
      }

      if (key.name === 'up') {
        setErrorMessage(null);
        moveOption('up');
        return;
      }

      if (key.name === 'down') {
        setErrorMessage(null);
        moveOption('down');
        return;
      }

      if (key.name === 'tab') {
        if (!showBashOptionInAuthDialog) {
          return;
        }
        setErrorMessage(null);
        moveSection(key.shift ? 'backward' : 'forward');
        return;
      }

      if (key.name === 'return') {
        if (activeOption === 'bash') {
          handleContinueToBash();
          return;
        }
        handleAuthSelect(activeOption);
      }
    },
    [
      errorMessage,
      config,
      onAuthSelect,
      moveOption,
      moveSection,
      activeOption,
      handleContinueToBash,
      handleAuthSelect,
      showBashOptionInAuthDialog,
    ],
  );

  useKeypress(handleDialogKeypress, { isActive: !isSelectingAuth });

  if (isSelectingAuth) {
    return (
      <Box
        borderStyle="round"
        borderColor={Colors.Gray}
        flexDirection="column"
        padding={1}
        width="100%"
      >
        <Text>
          <Spinner type="dots" />{' '}
          {t('Initializing model configuration, please wait...')}
        </Text>
      </Box>
    );
  }

  return (
    <Box
      borderStyle="round"
      borderColor={Colors.Gray}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold>
        {isManualAuthDialog ? t('Select authorization') : t('Get started')}
      </Text>
      {isManualAuthDialog && (
        <Box marginTop={1}>
          <Text>
            {t("Choose how you'd like to authenticate in Copilot Shell.")}
          </Text>
        </Box>
      )}
      <Box marginTop={1} flexDirection="column">
        {!isManualAuthDialog && (
          <Text
            bold
            color={activeSection === 'auth' ? Colors.AccentBlue : Colors.Gray}
          >
            {t('Use Copilot Shell')}
          </Text>
        )}
        <Box marginTop={1}>
          <DescriptiveRadioButtonSelect
            items={authItems}
            initialIndex={authSelectedIndex}
            activeIndexOverride={authDisplayIndex}
            onSelect={handleAuthSelect}
            onHighlight={handleHighlight}
            isFocused={false}
          />
        </Box>
      </Box>
      {showBashOptionInAuthDialog && (
        <Box marginTop={1} flexDirection="column">
          <Text
            bold
            color={activeSection === 'bash' ? Colors.AccentBlue : Colors.Gray}
          >
            {t('Continue without Copilot Shell')}
          </Text>
          <Box marginTop={1}>
            <DescriptiveRadioButtonSelect
              items={[bashItem]}
              initialIndex={0}
              activeIndexOverride={bashDisplayIndex}
              onSelect={handleContinueToBash}
              onHighlight={handleHighlight}
              isFocused={false}
            />
          </Box>
        </Box>
      )}
      {(authError || errorMessage) && (
        <Box marginTop={1}>
          <Text color={Colors.AccentRed}>{authError || errorMessage}</Text>
        </Box>
      )}
      <Box marginTop={1}>
        <Text color={Colors.AccentPurple}>
          {showBashOptionInAuthDialog
            ? t('(↑↓ Select · Tab Switch Section · Enter Continue)')
            : t('(↑↓ Select · Enter Continue)')}
        </Text>
      </Box>
    </Box>
  );
}
