pub(super) const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>COSH Tasks</title>
  <link rel="stylesheet" href="/app.css">
</head>
<body>
  <main>
    <h1>COSH Tasks</h1>
    <p class="notice">Local continuation beta. Keep this page on this computer.</p>
    <form id="login">
      <label>Bearer token <input id="token" type="password" autocomplete="off" required></label>
      <button>Connect</button>
    </form>
    <p id="status" role="status"></p>
    <section id="tasks" aria-live="polite"></section>
    <section id="detail" aria-live="polite"></section>
  </main>
  <script src="/app.js" defer></script>
</body>
</html>
"#;

pub(super) const APP_CSS: &str = r#"
:root { color-scheme: light dark; font-family: system-ui, sans-serif; }
body { margin: 0; background: #10151b; color: #e9eef4; }
main { max-width: 960px; margin: auto; padding: 2rem 1rem; }
.notice { color: #a9bad0; }
form, article { background: #18212b; padding: 1rem; border-radius: .6rem; margin: .8rem 0; }
input, button, select, textarea { font: inherit; padding: .55rem; margin: .25rem; }
button { cursor: pointer; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; background: #0c1117; padding: .8rem; }
.task { width: 100%; text-align: left; }
.approval { border-left: .25rem solid #e6a23c; }
.error { color: #ff8e8e; }
"#;

pub(super) const APP_JS: &str = r#"
(() => {
  'use strict';
  let token = '';
  let selected = null;
  let timer = null;
  let selectionGeneration = 0;
  let selectionState = null;
  const status = document.querySelector('#status');
  const tasks = document.querySelector('#tasks');
  const detail = document.querySelector('#detail');

  function key() { return crypto.randomUUID(); }
  async function api(path, options = {}) {
    const headers = new Headers(options.headers || {});
    headers.set('Authorization', `Bearer ${token}`);
    if (options.method && options.method !== 'GET') {
      headers.set('Content-Type', 'application/json');
      headers.set('Idempotency-Key', options.idempotencyKey || key());
    }
    const response = await fetch(path, {...options, headers, credentials: 'omit'});
    const payload = await response.json();
    if (!response.ok || !payload.ok) throw new Error(payload.error || `HTTP ${response.status}`);
    return payload.data;
  }

  function showError(error) {
    status.className = 'error';
    status.textContent = error instanceof Error ? error.message : String(error);
  }

  async function loadTasks() {
    const response = await api('/api/v1/tasks?limit=64');
    const page = response.data;
    tasks.replaceChildren();
    for (const task of page.tasks) {
      const button = document.createElement('button');
      button.className = 'task';
      button.textContent = `${task.state}  ${task.task_id}`;
      button.addEventListener('click', () => selectTask(task.task_id).catch(showError));
      tasks.append(button);
    }
    status.className = '';
    status.textContent = `Connected. ${page.tasks.length} task(s).`;
  }

  async function selectTask(taskId) {
    if (timer) clearTimeout(timer);
    const generation = ++selectionGeneration;
    selected = taskId;
    const state = {
      generation, taskId, cursor: 0, events: [],
      pendingApprovals: new Map(), pendingInputs: new Map()
    };
    selectionState = state;
    detail.replaceChildren();
    await renderTaskControls(state);
    await poll(state);
    schedulePoll(state);
  }

  function current(state) {
    return state === selectionState &&
      state.generation === selectionGeneration && state.taskId === selected;
  }

  async function renderTaskControls(state) {
    const response = await api(`/api/v1/tasks/${state.taskId}`);
    if (!current(state)) return;
    const task = response.data;
    const article = document.createElement('article');
    const heading = document.createElement('h2');
    heading.textContent = `${task.state}  ${task.task_id}`;
    article.append(heading);
    if (task.active_run_id && ['queued', 'running', 'waiting_approval', 'waiting_input', 'suspended'].includes(task.state)) {
      article.append(actionButton('Cancel run', async () => {
        await api(`/api/v1/tasks/${state.taskId}/cancel`, {
          method: 'POST',
          body: JSON.stringify({run_id: task.active_run_id, expected_revision: task.revision})
        });
        if (current(state)) await selectTask(state.taskId);
      }));
    }
    if (task.active_run_id && task.state === 'suspended') {
      article.append(actionButton('Retry suspended run', async () => {
        await api(`/api/v1/tasks/${state.taskId}/retry`, {
          method: 'POST',
          body: JSON.stringify({previous_run_id: task.active_run_id, expected_revision: task.revision})
        });
        if (current(state)) await selectTask(state.taskId);
      }));
    }
    detail.append(article);
  }

  function actionButton(label, action) {
    const button = document.createElement('button');
    button.textContent = label;
    button.addEventListener('click', async () => {
      button.disabled = true;
      try {
        await action();
      } catch (error) {
        button.disabled = false;
        showError(error);
      }
    });
    return button;
  }

  function reduceInteraction(state, envelope) {
    const event = envelope.event;
    if (event.event === 'approval_requested') {
      state.pendingApprovals.set(event.approval.approval_id, event.approval.run_id);
    } else if (event.event === 'approval_resolved') {
      state.pendingApprovals.delete(event.approval_id);
    } else if (event.event === 'input_requested') {
      state.pendingInputs.set(event.request.request_id, event.request.run_id);
    } else if (event.event === 'input_submitted') {
      state.pendingInputs.delete(event.request_id);
    } else if (['run_suspended', 'run_failed', 'run_cancelled', 'run_succeeded'].includes(event.event)) {
      clearRunInteractions(state, event.run_id);
    } else if (event.event === 'run_retry_queued') {
      clearRunInteractions(state, event.previous_run_id);
    } else if (['task_succeeded', 'task_failed', 'task_cancelled'].includes(event.event)) {
      state.pendingApprovals.clear();
      state.pendingInputs.clear();
    }
    state.events.push(envelope);
    if (state.events.length > 256) state.events.shift();
  }

  function clearRunInteractions(state, runId) {
    for (const [id, owner] of state.pendingApprovals) {
      if (owner === runId) state.pendingApprovals.delete(id);
    }
    for (const [id, owner] of state.pendingInputs) {
      if (owner === runId) state.pendingInputs.delete(id);
    }
  }

  function renderEvent(state, envelope) {
    const article = document.createElement('article');
    const pre = document.createElement('pre');
    pre.textContent = JSON.stringify(envelope.event, null, 2);
    article.append(pre);
    const event = envelope.event;
    if (event.event === 'approval_requested' &&
        state.pendingApprovals.has(event.approval.approval_id)) {
      article.className = 'approval';
      const approval = event.approval.approval_id;
      for (const decision of ['approve', 'deny']) {
        article.append(actionButton(decision, async () => {
          await api(`/api/v1/tasks/${state.taskId}/approvals/${approval}`, {
            method: 'POST', body: JSON.stringify({decision})
          });
          state.pendingApprovals.delete(approval);
          if (current(state)) renderTimeline(state);
          await poll(state);
        }));
      }
    }
    if (event.event === 'input_requested' &&
        state.pendingInputs.has(event.request.request_id)) {
      const input = document.createElement('textarea');
      input.setAttribute('aria-label', 'Answer');
      article.append(input, actionButton('Answer', async () => {
        await api(`/api/v1/tasks/${state.taskId}/inputs/${event.request.request_id}`, {
          method: 'POST', body: JSON.stringify({text: input.value})
        });
        state.pendingInputs.delete(event.request.request_id);
        if (current(state)) renderTimeline(state);
        await poll(state);
      }));
    }
    return article;
  }

  function renderTimeline(state) {
    if (!current(state)) return;
    let timeline = document.querySelector('#timeline');
    if (!timeline) {
      timeline = document.createElement('section');
      timeline.id = 'timeline';
      detail.append(timeline);
    }
    timeline.replaceChildren(...state.events.map(event => renderEvent(state, event)));
  }

  async function poll(state) {
    if (!current(state) || state.polling) return;
    state.polling = true;
    try {
      let hasMore = true;
      while (hasMore && current(state)) {
        const response = await api(
          `/api/v1/tasks/${state.taskId}/events?after=${state.cursor}&limit=64`
        );
        if (!current(state)) return;
        const page = response.data;
        for (const event of page.events) reduceInteraction(state, event);
        state.cursor = page.next_revision;
        hasMore = page.has_more;
      }
      renderTimeline(state);
    } finally {
      state.polling = false;
    }
  }

  function schedulePoll(state) {
    if (!current(state)) return;
    timer = setTimeout(async () => {
      try {
        await poll(state);
      } catch (error) {
        showError(error);
      }
      schedulePoll(state);
    }, 1000);
  }

  document.querySelector('#login').addEventListener('submit', event => {
    event.preventDefault();
    token = document.querySelector('#token').value;
    document.querySelector('#token').value = '';
    loadTasks().catch(showError);
  });
})();
"#;
