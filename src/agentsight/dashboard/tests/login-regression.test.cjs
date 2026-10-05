const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const test = require('node:test');
const apiClient = require(process.env.AGENTSIGHT_API_CLIENT_BUILD);

// Exercise the compiled page's event handlers with a persistent hook state.
// The element objects expose the same props React passes to the form/input.
function loginPage() {
  const states = [];
  let stateIndex = 0;
  let authenticated = 0;
  const exports = {};
  runInNewContext(readFileSync(process.env.AGENTSIGHT_LOGIN_PAGE_BUILD, 'utf8'), {
    exports,
    require(name) {
      if (name === 'react') return {
        useState(initial) {
          const index = stateIndex++;
          if (!(index in states)) states[index] = initial;
          return [states[index], (value) => { states[index] = value; }];
        },
      };
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }),
        jsxs: (type, props) => ({ type, props }),
      };
      if (name === '../i18n') return {
        useI18n: () => ({ t: (key) => key }),
        LanguageSwitcher: () => null,
      };
      if (name === '../utils/apiClient') return apiClient;
      throw new Error(`Unexpected LoginPage dependency: ${name}`);
    },
  });
  function render() {
    stateIndex = 0;
    return exports.LoginPage({ onAuthenticated: () => { authenticated++; } });
  }
  function find(node, predicate) {
    if (!node || typeof node !== 'object') return undefined;
    if (predicate(node)) return node;
    for (const child of [node.props?.children].flat()) {
      const result = find(child, predicate);
      if (result) return result;
    }
  }
  return {
    input(value) {
      find(render(), (node) => node.type === 'input').props.onChange({ target: { value } });
    },
    async submit() {
      let prevented = false;
      await find(render(), (node) => node.type === 'form').props.onSubmit({
        preventDefault() { prevented = true; },
      });
      assert.equal(prevented, true);
    },
    error: () => find(render(), (node) => node.props?.className?.startsWith('text-red-600'))?.props.children,
    disabled: () => find(render(), (node) => node.type === 'button').props.disabled,
    authenticated: () => authenticated,
  };
}

test('login accepts a successful response and preserves the cookie request', async (t) => {
  let request;
  t.mock.method(global, 'fetch', async (url, init) => {
    request = { url, init };
    return new Response(null, { status: 200 });
  });
  assert.equal(await apiClient.login('valid-token'), true);
  assert.equal(new URL(request.url, 'http://localhost').pathname, '/api/auth/login');
  assert.deepEqual(request.init, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify({ token: 'valid-token' }),
  });
});

test('login returns false only for the backend invalid-token response', async (t) => {
  t.mock.method(global, 'fetch', async () => new Response(null, { status: 401 }));
  assert.equal(await apiClient.login('invalid-token'), false);
});

test('login rejects other HTTP failures with the response status', async (t) => {
  let status;
  t.mock.method(global, 'fetch', async () => new Response(null, { status }));
  for (status of [403, 429, 500, 503]) {
    await assert.rejects(apiClient.login('valid-token'), new RegExp(`POST /api/auth/login -> ${status}`));
  }
});

test('login page distinguishes invalid tokens from HTTP and network failures', async (t) => {
  let response;
  t.mock.method(global, 'fetch', async () => {
    if (response instanceof Error) throw response;
    return new Response(null, { status: response });
  });
  const page = loginPage();
  page.input('valid-token');
  for (const [result, expected] of [
    [401, 'login.error.invalid'],
    [500, 'login.error.connection'],
    [503, 'login.error.connection'],
    [new TypeError('fetch failed'), 'login.error.connection'],
  ]) {
    response = result;
    await page.submit();
    assert.equal(page.error(), expected);
    assert.equal(page.authenticated(), 0);
    assert.equal(page.disabled(), false);
  }
});

test('login page validates empty input and trims a successful login', async (t) => {
  const requests = [];
  t.mock.method(global, 'fetch', async (_url, init) => {
    requests.push(JSON.parse(init.body));
    return new Response(null, { status: 200 });
  });
  const page = loginPage();
  page.input('  ');
  await page.submit();
  assert.equal(page.error(), 'login.error.required');
  assert.equal(requests.length, 0);
  page.input('  valid-token  ');
  await page.submit();
  assert.deepEqual(requests, [{ token: 'valid-token' }]);
  assert.equal(page.authenticated(), 1);
  assert.equal(page.error(), undefined);
  assert.equal(page.disabled(), false);
});
