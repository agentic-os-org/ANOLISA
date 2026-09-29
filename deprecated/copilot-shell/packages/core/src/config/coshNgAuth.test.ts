/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  COSH_NG_CONFIG_FILE_NAME,
  getCoshNgConfigPath,
  loadCoshNgAuth,
} from './coshNgAuth.js';

describe('loadCoshNgAuth', () => {
  let configDir: string;
  let warnSpy: ReturnType<typeof vi.spyOn>;

  const writeConfig = (content: string) => {
    fs.writeFileSync(
      path.join(configDir, COSH_NG_CONFIG_FILE_NAME),
      content,
      'utf-8',
    );
  };

  /** All warning text emitted so far, joined for leak assertions. */
  const warnings = () => warnSpy.mock.calls.map((call) => String(call[0]));

  /** Runs `body` with the given environment, restoring it afterwards. */
  const withEnv = (
    env: Record<string, string | undefined>,
    body: () => void,
  ) => {
    const saved = new Map(
      Object.keys(env).map((key) => [key, process.env[key]]),
    );
    try {
      for (const [key, value] of Object.entries(env)) {
        if (value === undefined) delete process.env[key];
        else process.env[key] = value;
      }
      body();
    } finally {
      for (const [key, value] of saved) {
        if (value === undefined) delete process.env[key];
        else process.env[key] = value;
      }
    }
  };

  // cosh-ng reads several credential fields from the environment when the TOML
  // omits them, so the ambient environment has to be pinned for these tests to
  // mean anything.
  const AMBIENT_CREDENTIAL_VARS = [
    'ALIBABA_CLOUD_SECURITY_TOKEN',
    'ALIBABA_CLOUD_ACCESS_KEY_ID',
    'ALIBABA_CLOUD_ACCESS_KEY_SECRET',
  ];
  let savedAmbientEnv: Array<[string, string | undefined]> = [];

  beforeEach(() => {
    configDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cosh-ng-auth-'));
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    savedAmbientEnv = AMBIENT_CREDENTIAL_VARS.map((key) => [
      key,
      process.env[key],
    ]);
    for (const key of AMBIENT_CREDENTIAL_VARS) {
      delete process.env[key];
    }
  });

  afterEach(() => {
    fs.rmSync(configDir, { recursive: true, force: true });
    warnSpy.mockRestore();
    for (const [key, value] of savedAmbientEnv) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });

  describe('OpenAI-compatible providers', () => {
    it('reads a dashscope provider', () => {
      writeConfig(`
[ai]
active_provider = "dashscope"
active_model = "qwen3-235b-a22b"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "sk-dashscope"
model = "qwen-max"
`);

      expect(loadCoshNgAuth(configDir)).toEqual({
        kind: 'openai',
        apiKey: 'sk-dashscope',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        // active_model wins over the provider's own model.
        model: 'qwen3-235b-a22b',
      });
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it.each([
      ['openai', 'type = "openai"'],
      ['openai_compat', 'type = "openai_compat"'],
      ['deepseek', 'type = "deepseek"'],
      ['generic', 'type = "generic"'],
      // cosh-ng defaults an omitted type to "generic".
      ['omitted', ''],
    ])('accepts the %s provider type', (_name, typeLine) => {
      writeConfig(`
[ai]
active_provider = "p"

[ai.providers.p]
${typeLine}
base_url = "https://example.com/v1"
api_key = "sk-example"
model = "some-model"
`);

      expect(loadCoshNgAuth(configDir)).toEqual({
        kind: 'openai',
        apiKey: 'sk-example',
        baseUrl: 'https://example.com/v1',
        model: 'some-model',
      });
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it('falls back to the provider model when active_model is absent', () => {
      writeConfig(`
[ai]
active_provider = "custom"

[ai.providers.custom]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-custom"
model = "my-model"
`);

      expect(loadCoshNgAuth(configDir)).toMatchObject({ model: 'my-model' });
    });

    it('is usable without any model, which is not a credential', () => {
      writeConfig(`
[ai]
active_provider = "custom"

[ai.providers.custom]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-custom"
`);

      expect(loadCoshNgAuth(configDir)).toEqual({
        kind: 'openai',
        apiKey: 'sk-custom',
        baseUrl: 'https://example.com/v1',
        model: undefined,
      });
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it('warns by field name when a credential is missing', () => {
      writeConfig(`
[ai]
active_provider = "partial"

[ai.providers.partial]
type = "openai_compat"
api_key = "sk-partial-secret"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnings().join('\n')).toContain('missing base_url');
      expect(warnings().join('\n')).not.toContain('sk-partial-secret');
    });

    it('treats blank credential values as missing', () => {
      writeConfig(`
[ai]
active_provider = "blank"

[ai.providers.blank]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "   "
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnings().join('\n')).toContain('missing api_key');
    });
  });

  describe('Aliyun providers', () => {
    it('reads AK/SK credentials', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "LTAI-test"
access_key_secret = "secret-test"
model = "qwen3.7-plus"
`);

      expect(loadCoshNgAuth(configDir)).toEqual({
        kind: 'aliyun',
        accessKeyId: 'LTAI-test',
        accessKeySecret: 'secret-test',
        model: 'qwen3.7-plus',
      });
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it('refuses temporary STS credentials, which cannot stay read-only', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "STS.test"
access_key_secret = "secret-test"
security_token = "token-test"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('temporary STS credential');
      expect(message).not.toContain('secret-test');
      expect(message).not.toContain('token-test');
    });

    // cosh-ng falls back to $ALIBABA_CLOUD_SECURITY_TOKEN when the provider
    // section omits security_token, so a temporary AK/SK can be token-bearing
    // with nothing in the TOML to show it. Inheriting it would send every
    // request without the token.
    it('refuses an STS token supplied through the environment', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "STS.test"
access_key_secret = "secret-test"
`);

      withEnv({ ALIBABA_CLOUD_SECURITY_TOKEN: 'token-from-env-secret' }, () => {
        expect(loadCoshNgAuth(configDir)).toBeUndefined();
        const message = warnings().join('\n');
        expect(message).toContain('temporary STS credential');
        expect(message).not.toContain('token-from-env-secret');
        expect(message).not.toContain('secret-test');
        expect(message).not.toContain('ALIBABA_CLOUD_SECURITY_TOKEN');
      });
    });

    it('refuses a blank STS token from the environment', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "LTAI-test"
access_key_secret = "secret-test"
`);

      withEnv({ ALIBABA_CLOUD_SECURITY_TOKEN: '' }, () => {
        expect(loadCoshNgAuth(configDir)).toBeUndefined();
        expect(warnings().join('\n')).toContain('temporary STS credential');
      });
    });

    it('is unaffected by an STS token when the provider is OpenAI-compatible', () => {
      writeConfig(`
[ai]
active_provider = "p"

[ai.providers.p]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-test"
`);

      withEnv({ ALIBABA_CLOUD_SECURITY_TOKEN: 'token-from-env' }, () => {
        expect(loadCoshNgAuth(configDir)).toMatchObject({ kind: 'openai' });
        expect(warnSpy).not.toHaveBeenCalled();
      });
    });

    it('refuses a blank security_token rather than treating it as absent', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "LTAI-test"
access_key_secret = "secret-test"
security_token = ""
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnings().join('\n')).toContain('temporary STS credential');
    });

    it('refuses the ECS RAM role flow, which has no equivalent here', () => {
      writeConfig(`
[ai]
active_provider = "sysom-trial"

[ai.providers.sysom-trial]
type = "aliyun"
auth_source = "ecs_ram_role"
access_key_id = "STS.test"
access_key_secret = "secret-test"
security_token = "token-test"
model = "qwen3.7-plus"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('ECS RAM role');
      expect(message).not.toContain('secret-test');
      expect(message).not.toContain('token-test');
    });

    it('warns by field name when AK/SK is incomplete', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "LTAI-test"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnings().join('\n')).toContain('missing access_key_secret');
    });
  });

  // cosh-ng expands ${VAR} in credentials and base URLs, treating an undefined
  // variable as empty. Mirroring that here keeps a config that works under
  // cosh-ng working after cosh-switch, and turns an unresolvable reference into
  // a refusal instead of a literal "${VAR}" reaching a provider.
  describe('${VAR} expansion', () => {
    it('expands defined references in OpenAI credentials', () => {
      writeConfig(`
[ai]
active_provider = "envy"

[ai.providers.envy]
type = "openai_compat"
base_url = "https://\${COSH_TEST_HOST}/v1"
api_key = "\${COSH_TEST_KEY}"
model = "some-model"
`);

      withEnv(
        { COSH_TEST_KEY: 'sk-from-env', COSH_TEST_HOST: 'env.example.com' },
        () => {
          expect(loadCoshNgAuth(configDir)).toEqual({
            kind: 'openai',
            apiKey: 'sk-from-env',
            baseUrl: 'https://env.example.com/v1',
            model: 'some-model',
          });
          expect(warnSpy).not.toHaveBeenCalled();
        },
      );
    });

    it('refuses an OpenAI provider whose reference resolves to nothing', () => {
      writeConfig(`
[ai]
active_provider = "envy"

[ai.providers.envy]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "\${COSH_TEST_MISSING}"
`);

      withEnv({ COSH_TEST_MISSING: undefined }, () => {
        expect(loadCoshNgAuth(configDir)).toBeUndefined();
        const message = warnings().join('\n');
        expect(message).toContain('missing api_key');
        // The referenced variable name is itself user input; never echo it.
        expect(message).not.toContain('COSH_TEST_MISSING');
      });
    });

    it('expands defined references in Aliyun credentials', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "\${COSH_TEST_AK}"
access_key_secret = "\${COSH_TEST_SK}"
`);

      withEnv(
        { COSH_TEST_AK: 'LTAI-from-env', COSH_TEST_SK: 'sk-from-env' },
        () => {
          expect(loadCoshNgAuth(configDir)).toEqual({
            kind: 'aliyun',
            accessKeyId: 'LTAI-from-env',
            accessKeySecret: 'sk-from-env',
            model: undefined,
          });
          expect(warnSpy).not.toHaveBeenCalled();
        },
      );
    });

    it('refuses an Aliyun provider whose reference resolves to nothing', () => {
      writeConfig(`
[ai]
active_provider = "aliyun"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "LTAI-test"
access_key_secret = "\${COSH_TEST_MISSING}"
`);

      withEnv({ COSH_TEST_MISSING: undefined }, () => {
        expect(loadCoshNgAuth(configDir)).toBeUndefined();
        expect(warnings().join('\n')).toContain('missing access_key_secret');
      });
    });

    it('leaves the model unexpanded, as cosh-ng does', () => {
      writeConfig(`
[ai]
active_provider = "envy"
active_model = "\${COSH_TEST_MODEL}"

[ai.providers.envy]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-test"
`);

      withEnv({ COSH_TEST_MODEL: 'model-from-env' }, () => {
        expect(loadCoshNgAuth(configDir)).toMatchObject({
          model: '${COSH_TEST_MODEL}',
        });
      });
    });

    it('leaves an unterminated ${ alone', () => {
      writeConfig(`
[ai]
active_provider = "envy"

[ai.providers.envy]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-\${unterminated"
`);

      expect(loadCoshNgAuth(configDir)).toMatchObject({
        apiKey: 'sk-${unterminated',
      });
    });

    it('does not re-expand a substituted value', () => {
      writeConfig(`
[ai]
active_provider = "envy"

[ai.providers.envy]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "\${COSH_TEST_NESTED}"
`);

      withEnv(
        { COSH_TEST_NESTED: '${COSH_TEST_INNER}', COSH_TEST_INNER: 'inner' },
        () => {
          expect(loadCoshNgAuth(configDir)).toMatchObject({
            apiKey: '${COSH_TEST_INNER}',
          });
        },
      );
    });
  });

  describe('unusable configurations', () => {
    it('returns undefined when config.toml does not exist', () => {
      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it('warns without leaking the error when config.toml is unreadable', () => {
      const configPath = getCoshNgConfigPath(configDir);
      fs.mkdirSync(configPath);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();

      const message = warnings().join('\n');
      expect(message).toContain('Could not read');
      expect(message).toContain(configPath);
      expect(message).not.toContain('EISDIR');
      expect(message).not.toContain('illegal operation');
    });

    it('returns undefined without warning when there is no active provider', () => {
      writeConfig(`
[ai]
active_model = "qwen-max"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnSpy).not.toHaveBeenCalled();
    });

    it('warns when the active provider has no section', () => {
      writeConfig(`
[ai]
active_provider = "ghost"

[ai.providers.other]
type = "openai_compat"
base_url = "https://example.com/v1"
api_key = "sk-other-secret"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('no matching [ai.providers] section');
      expect(message).not.toContain('ghost');
      expect(message).not.toContain('sk-other-secret');
    });

    it('warns for provider types with no equivalent here', () => {
      writeConfig(`
[ai]
active_provider = "fake"

[ai.providers.fake]
type = "mock"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnings().join('\n')).toContain('unsupported provider type');
    });

    // A pasted credential consists only of characters a provider id may
    // legitimately contain, so identifier values must never be logged at all.
    it('never logs the active provider id', () => {
      writeConfig(`
[ai]
active_provider = "sk-live-secret-leak-check"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('The active provider');
      expect(message).not.toContain('sk-live-secret-leak-check');
    });

    it('never logs the provider type', () => {
      writeConfig(`
[ai]
active_provider = "p"

[ai.providers.p]
type = "sk-live-secret-leak-check"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('unsupported provider type');
      expect(message).not.toContain('sk-live-secret-leak-check');
    });

    it('never leaks credentials when the TOML is malformed', () => {
      // A truncated string literal makes the parser point straight at the
      // api_key line, which is exactly what must not reach the log.
      writeConfig(`
[ai]
active_provider = "p"

[ai.providers.p]
api_key = "sk-fake-secret
base_url = "https://example.com/v1"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      const message = warnings().join('\n');
      expect(message).toContain('malformed');
      expect(message).toContain(getCoshNgConfigPath(configDir));
      expect(message).not.toContain('sk-fake-secret');
      expect(message).not.toContain('api_key');
      expect(message).not.toContain('row');
    });

    it('ignores a config with no [ai] table', () => {
      writeConfig(`
[agent]
approval_mode = "default"
`);

      expect(loadCoshNgAuth(configDir)).toBeUndefined();
      expect(warnSpy).not.toHaveBeenCalled();
    });
  });

  it('never writes to the cosh-ng config file', () => {
    const configPath = getCoshNgConfigPath(configDir);
    writeConfig(`
[ai]
active_provider = "dashscope"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "sk-dashscope"
model = "qwen-max"
`);
    const before = fs.readFileSync(configPath, 'utf-8');
    const mtimeBefore = fs.statSync(configPath).mtimeMs;

    expect(loadCoshNgAuth(configDir)).toBeDefined();

    expect(fs.readFileSync(configPath, 'utf-8')).toBe(before);
    expect(fs.statSync(configPath).mtimeMs).toBe(mtimeBefore);
    expect(fs.readdirSync(configDir)).toEqual([COSH_NG_CONFIG_FILE_NAME]);
  });
});
