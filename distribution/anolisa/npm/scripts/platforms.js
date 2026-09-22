/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

// Publishing and postinstall share this list so platform identities cannot drift.
export const TARGETS = Object.freeze([
  {
    rust_target: 'x86_64-unknown-linux-gnu',
    npm_os: 'linux',
    npm_cpu: 'x64',
    pkg_suffix: 'linux-x64',
  },
  {
    rust_target: 'aarch64-unknown-linux-gnu',
    npm_os: 'linux',
    npm_cpu: 'arm64',
    pkg_suffix: 'linux-arm64',
  },
  {
    rust_target: 'aarch64-apple-darwin',
    npm_os: 'darwin',
    npm_cpu: 'arm64',
    pkg_suffix: 'darwin-arm64',
  },
]);

export const PLATFORM_MAP = Object.freeze(
  Object.fromEntries(
    TARGETS.map((target) => [
      `${target.npm_os}-${target.npm_cpu}`,
      `@anolisa/cli-${target.pkg_suffix}`,
    ]),
  ),
);

export function platformPackageName(npmOs, npmCpu) {
  const key = `${npmOs}-${npmCpu}`;
  const packageName = PLATFORM_MAP[key];
  if (!packageName) {
    throw new Error(`No prebuilt binary is available for ${key}`);
  }
  return packageName;
}

export function targetsForHost(hostPlatform) {
  const targets = TARGETS.filter((target) => target.npm_os === hostPlatform);
  if (targets.length === 0) {
    throw new Error(`No npm build targets are supported on ${hostPlatform}`);
  }
  return targets;
}

export function buildStrategyForTarget(
  hostPlatform,
  hostRustTarget,
  target,
) {
  if (target.npm_os !== hostPlatform) {
    throw new Error(
      `Cannot build ${target.pkg_suffix} on ${hostPlatform}; use a ${target.npm_os} runner`,
    );
  }
  if (target.rust_target === hostRustTarget) {
    return { tool: 'cargo', passTarget: false };
  }
  if (hostPlatform === 'linux') {
    return { tool: 'cross', passTarget: true };
  }
  return { tool: 'cargo', passTarget: true };
}

export function targetForSelector(selector) {
  const exact = TARGETS.filter(
    (target) =>
      target.pkg_suffix === selector || target.rust_target === selector,
  );
  if (exact.length === 1) {
    return exact[0];
  }

  const aliases = TARGETS.filter(
    (target) =>
      target.npm_cpu === selector ||
      target.rust_target.split('-', 1)[0] === selector,
  );
  if (aliases.length === 1) {
    return aliases[0];
  }
  if (aliases.length > 1) {
    throw new Error(
      `Target "${selector}" is ambiguous; use one of: ${aliases.map((target) => target.pkg_suffix).join(', ')}`,
    );
  }
  throw new Error(
    `Unknown target "${selector}"; available targets: ${TARGETS.map((target) => target.pkg_suffix).join(', ')}`,
  );
}
