import {readFile} from 'node:fs/promises';
import path from 'node:path';
import Ajv2020 from 'ajv/dist/2020.js';
import {generatedDir, websiteDir} from './lib.mjs';

const schema = JSON.parse(await readFile(path.join(websiteDir, 'agent-index/schema.json'), 'utf8'));
const index = JSON.parse(await readFile(path.join(generatedDir, 'static/agents/repo-index.json'), 'utf8'));
const ajv = new Ajv2020({allErrors: true});
const validate = ajv.compile(schema);

if (!validate(index)) {
  console.error('Agent index schema validation failed:');
  for (const error of validate.errors || []) {
    console.error(`- ${error.instancePath || '/'} ${error.message}`);
  }
  process.exit(1);
}

if (index.setup_workflow.host_detection.some((command) => command.startsWith('anolisa '))) {
  console.error('Host detection must not depend on the ANOLISA CLI.');
  process.exit(1);
}
if (
  index.setup_workflow.host_aliases.os.Darwin !== 'macos' ||
  index.setup_workflow.host_aliases.architectures.arm64 !== 'aarch64'
) {
  console.error('Host aliases must normalize Darwin/arm64 to macos/aarch64.');
  process.exit(1);
}

const supportedOperatingSystems = new Set(
  index.setup_workflow.supported_operating_systems,
);
const canonicalArchitectures = new Set(
  Object.values(index.setup_workflow.host_aliases.architectures),
);

function targetsOverlap(left, right) {
  if (left.os !== right.os) return false;
  if (left.architectures.length === 0 || right.architectures.length === 0) {
    return true;
  }
  return left.architectures.some(
    (architecture) => right.architectures.includes(architecture),
  );
}

function targetCoveredBy(candidate, target) {
  return candidate.os === target.os && (
    candidate.architectures.length === 0 ||
    target.architectures.every(
      (architecture) => candidate.architectures.includes(architecture),
    )
  );
}

for (const component of index.components) {
  if (component.platform_support.windows) {
    console.error(`${component.id}: the Agent setup contract does not support Windows.`);
    process.exit(1);
  }

  const preferredVariants = component.install_variants.filter(
    (variant) => variant.preferred,
  );
  if (preferredVariants.length !== 1) {
    console.error(
      `${component.id}: exactly one install variant must be preferred.`,
    );
    process.exit(1);
  }
  const preferredVariant = preferredVariants[0];
  const preferredEntry =
    preferredVariant.command || preferredVariant.documentation_url;
  if (
    preferredVariant.method !== component.install_method ||
    preferredEntry !== component.install
  ) {
    console.error(`${component.id}: the preferred variant must match the default install entry.`);
    process.exit(1);
  }

  for (const variant of component.install_variants) {
    if (
      variant.method !== 'manual' &&
      component.id !== 'anolisa' &&
      component.id !== 'tokenless'
    ) {
      console.error(
        `${component.id}: automated Agent setup is not published for this component yet.`,
      );
      process.exit(1);
    }
    if (variant.method === 'cli' && !variant.requires.includes('anolisa')) {
      console.error(`${component.id}: CLI variants must require anolisa.`);
      process.exit(1);
    }
    if (
      variant.method === 'npm' &&
      (!variant.requires.includes('node') || !variant.requires.includes('npm'))
    ) {
      console.error(`${component.id}: npm variants must require node and npm.`);
      process.exit(1);
    }
    if (
      variant.method === 'npm' &&
      [...variant.preflight, ...variant.verify].some(
        (command) => command.startsWith('anolisa '),
      )
    ) {
      console.error(
        `${component.id}: ${variant.method} workflows must not use ANOLISA lifecycle commands.`,
      );
      process.exit(1);
    }
    if (variant.method !== 'manual' && variant.verify.length === 0) {
      console.error(`${component.id}: ${variant.method} variants need verification commands.`);
      process.exit(1);
    }
    if (
      variant.method === 'manual' &&
      (variant.requires.length > 0 ||
        variant.preflight.length > 0 ||
        variant.verify.length > 0)
    ) {
      console.error(`${component.id}: manual variants must only point to documentation.`);
      process.exit(1);
    }

    for (const target of variant.platforms) {
      if (!supportedOperatingSystems.has(target.os)) {
        console.error(`${component.id}: Agent setup does not publish ${target.os} targets.`);
        process.exit(1);
      }
      if (!component.platform_support[target.os]) {
        console.error(
          `${component.id}: ${variant.method} targets unsupported OS ${target.os}.`,
        );
        process.exit(1);
      }
      const nonCanonicalArchitecture = target.architectures.find(
        (architecture) => !canonicalArchitectures.has(architecture),
      );
      if (nonCanonicalArchitecture) {
        console.error(
          `${component.id}: target architecture ${nonCanonicalArchitecture} is not canonical.`,
        );
        process.exit(1);
      }
      const supportedArchitectures = component.platform_support.architectures;
      const unsupportedArchitecture = target.architectures.find(
        (architecture) =>
          supportedArchitectures.length > 0 &&
          !supportedArchitectures.includes(architecture),
      );
      if (unsupportedArchitecture) {
        console.error(
          `${component.id}: ${variant.method} targets unsupported architecture ` +
          `${unsupportedArchitecture}.`,
        );
        process.exit(1);
      }
    }
  }

  for (let left = 0; left < component.install_variants.length; left += 1) {
    for (let right = left + 1; right < component.install_variants.length; right += 1) {
      const leftVariant = component.install_variants[left];
      const rightVariant = component.install_variants[right];
      const overlaps = leftVariant.platforms.some(
        (leftTarget) => rightVariant.platforms.some(
          (rightTarget) => targetsOverlap(leftTarget, rightTarget),
        ),
      );
      if (overlaps && leftVariant.preferred === rightVariant.preferred) {
        console.error(
          `${component.id}: overlapping variants need one unambiguous preferred entry.`,
        );
        process.exit(1);
      }
    }
  }

  for (const operatingSystem of supportedOperatingSystems) {
    if (
      component.platform_support[operatingSystem] &&
      !component.install_variants.some(
        (variant) => variant.platforms.some(
          (target) => target.os === operatingSystem,
        ),
      )
    ) {
      console.error(
        `${component.id}: ${operatingSystem} support has no reachable setup entry.`,
      );
      process.exit(1);
    }
  }
}

const bootstrapTargets = index.components
  .find((component) => component.id === 'anolisa')
  ?.install_variants
  .filter((variant) => variant.method === 'bootstrap')
  .flatMap((variant) => variant.platforms) || [];
for (const component of index.components) {
  for (const variant of component.install_variants.filter(
    (candidate) => candidate.method === 'cli',
  )) {
    for (const target of variant.platforms) {
      if (!bootstrapTargets.some((candidate) => targetCoveredBy(candidate, target))) {
        console.error(
          `${component.id}: CLI target ${target.os}/${target.architectures.join(',')}` +
          ' cannot bootstrap the ANOLISA CLI.',
        );
        process.exit(1);
      }
    }
  }
}

console.log(`Agent index schema validation passed for ${index.components.length} components.`);
