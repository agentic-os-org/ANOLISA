/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * HTML template for chat export.
 * Uses embedded styles and JavaScript for a self-contained export file.
 */
export const HTML_TEMPLATE = `<!DOCTYPE html>
<html lang="en" class="dark">

<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Copilot Shell Chat Export</title>

  <style>
    :root {
      --bg-primary: #18181b;
      --bg-secondary: #27272a;
      --bg-user: #3b82f6;
      --bg-assistant: #27272a;
      --text-primary: #f4f4f5;
      --text-secondary: #a1a1aa;
      --border-color: #3f3f46;
      --accent-color: #3b82f6;
    }

    body {
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
      margin: 0;
      padding: 0;
      background-color: var(--bg-primary);
      color: var(--text-primary);
      line-height: 1.6;
      -webkit-font-smoothing: antialiased;
    }

    .page-wrapper {
      min-height: 100vh;
      display: flex;
      flex-direction: column;
      align-items: center;
    }

    .header {
      width: 100%;
      padding: 16px 24px;
      border-bottom: 1px solid var(--border-color);
      background-color: rgba(24, 24, 27, 0.95);
      backdrop-filter: blur(8px);
      position: sticky;
      top: 0;
      z-index: 100;
      display: flex;
      justify-content: space-between;
      align-items: center;
      box-sizing: border-box;
    }

    .header-left {
      display: flex;
      align-items: center;
      gap: 12px;
    }

    .logo {
      font-size: 20px;
      font-weight: 700;
      color: var(--accent-color);
    }

    .meta {
      display: flex;
      gap: 24px;
      font-size: 13px;
      color: var(--text-secondary);
    }

    .meta-item {
      display: flex;
      align-items: center;
      gap: 8px;
    }

    .meta-label {
      color: #71717a;
    }

    .chat-container {
      width: 100%;
      max-width: 900px;
      padding: 40px 20px;
      box-sizing: border-box;
      flex: 1;
    }

    .message {
      margin-bottom: 24px;
      border-radius: 12px;
      overflow: hidden;
    }

    .message-header {
      padding: 12px 16px;
      font-weight: 600;
      font-size: 14px;
      display: flex;
      align-items: center;
      gap: 8px;
    }

    .message-content {
      padding: 16px;
      background-color: var(--bg-secondary);
      border-radius: 0 0 12px 12px;
    }

    .message.user .message-header {
      background-color: var(--bg-user);
    }

    .message.assistant .message-header {
      background-color: var(--bg-assistant);
      border: 1px solid var(--border-color);
      border-bottom: none;
      border-radius: 12px 12px 0 0;
    }

    .message.assistant .message-content {
      border: 1px solid var(--border-color);
      border-top: none;
    }

    .message.tool_call .message-header {
      background-color: #7c3aed;
    }

    .message.system .message-header {
      background-color: #52525b;
    }

    .text-content {
      white-space: pre-wrap;
      word-wrap: break-word;
    }

    .tool-info {
      font-size: 12px;
      color: var(--text-secondary);
      margin-top: 8px;
    }

    .tool-status {
      display: inline-block;
      padding: 2px 8px;
      border-radius: 4px;
      font-size: 11px;
      font-weight: 600;
    }

    .tool-status.completed {
      background-color: #22c55e;
      color: white;
    }

    .tool-status.failed {
      background-color: #ef4444;
      color: white;
    }

    .tool-status.in_progress {
      background-color: #f59e0b;
      color: white;
    }

    .tool-status.pending {
      background-color: #6b7280;
      color: white;
    }

    .code-block {
      background-color: #1e1e1e;
      border-radius: 8px;
      padding: 12px;
      margin: 8px 0;
      overflow-x: auto;
    }

    .code-block pre {
      margin: 0;
      font-size: 13px;
    }

    /* Scrollbar styling */
    ::-webkit-scrollbar {
      width: 10px;
      height: 10px;
    }

    ::-webkit-scrollbar-track {
      background: var(--bg-primary);
    }

    ::-webkit-scrollbar-thumb {
      background: var(--bg-secondary);
      border-radius: 5px;
      border: 2px solid var(--bg-primary);
    }

    ::-webkit-scrollbar-thumb:hover {
      background: #52525b;
    }

    /* Responsive adjustments */
    @media (max-width: 768px) {
      .chat-container {
        max-width: 100%;
        padding: 20px 16px;
      }

      .header {
        padding: 12px 16px;
        flex-direction: column;
        align-items: flex-start;
        gap: 12px;
      }

      .header-left {
        width: 100%;
        justify-content: space-between;
      }

      .meta {
        width: 100%;
        flex-direction: column;
        gap: 6px;
      }
    }

    @media (max-width: 480px) {
      .chat-container {
        padding: 16px 12px;
      }
    }
  </style>
</head>

<body>
  <div class="page-wrapper">
    <div class="header">
      <div class="header-left">
        <div class="logo">Copilot Shell</div>
      </div>
      <div class="meta">
        <div class="meta-item">
          <span class="meta-label">Session Id</span>
          <span id="session-id" class="font-mono">-</span>
        </div>
        <div class="meta-item">
          <span class="meta-label">Export Time</span>
          <span id="session-date">-</span>
        </div>
      </div>
    </div>

    <div id="chat-container" class="chat-container"></div>
  </div>

  <script id="chat-data" type="application/json">
    // DATA_PLACEHOLDER: Chat export data will be injected here
  </script>

  <script>
    const chatDataElement = document.getElementById('chat-data');
    const chatData = chatDataElement?.textContent
      ? JSON.parse(chatDataElement.textContent)
      : {};
    const rawMessages = Array.isArray(chatData.messages) ? chatData.messages : [];
    const messages = rawMessages.filter((record) => record && record.type !== 'system');

    // Populate metadata
    const sessionIdElement = document.getElementById('session-id');
    if (sessionIdElement && chatData.sessionId) {
      sessionIdElement.textContent = chatData.sessionId;
    }

    const sessionDateElement = document.getElementById('session-date');
    if (sessionDateElement && chatData.startTime) {
      try {
        const date = new Date(chatData.startTime);
        sessionDateElement.textContent = date.toLocaleString(undefined, {
          year: 'numeric',
          month: 'short',
          day: 'numeric',
          hour: '2-digit',
          minute: '2-digit'
        });
      } catch (e) {
        sessionDateElement.textContent = chatData.startTime;
      }
    }

    // Render messages
    const container = document.getElementById('chat-container');

    function escapeHtml(text) {
      const div = document.createElement('div');
      div.textContent = text;
      return div.innerHTML;
    }

    function extractText(message) {
      if (!message.message?.parts) return '';
      return message.message.parts
        .filter(part => 'text' in part)
        .map(part => part.text)
        .join('\\n');
    }

    function renderMessage(message) {
      const div = document.createElement('div');
      div.className = 'message ' + message.type;

      const header = document.createElement('div');
      header.className = 'message-header';

      const content = document.createElement('div');
      content.className = 'message-content';

      if (message.type === 'user') {
        header.textContent = 'User';
        const textDiv = document.createElement('div');
        textDiv.className = 'text-content';
        textDiv.textContent = extractText(message);
        content.appendChild(textDiv);
      } else if (message.type === 'assistant') {
        header.textContent = 'Assistant';
        if (message.model) {
          const modelSpan = document.createElement('span');
          modelSpan.style.fontSize = '12px';
          modelSpan.style.color = 'var(--text-secondary)';
          modelSpan.textContent = ' (' + message.model + ')';
          header.appendChild(modelSpan);
        }
        const textDiv = document.createElement('div');
        textDiv.className = 'text-content';
        textDiv.textContent = extractText(message);
        content.appendChild(textDiv);
      } else if (message.type === 'tool_call') {
        const title = typeof message.toolCall?.title === 'string'
          ? message.toolCall.title
          : JSON.stringify(message.toolCall?.title || 'Unknown Tool');
        header.textContent = 'Tool: ' + title;

        if (message.toolCall) {
          const infoDiv = document.createElement('div');
          infoDiv.className = 'tool-info';

          const statusSpan = document.createElement('span');
          statusSpan.className = 'tool-status ' + message.toolCall.status;
          statusSpan.textContent = message.toolCall.status;
          infoDiv.appendChild(statusSpan);

          if (message.toolCall.content && message.toolCall.content.length > 0) {
            const codeDiv = document.createElement('div');
            codeDiv.className = 'code-block';
            const pre = document.createElement('pre');

            for (const item of message.toolCall.content) {
              if (item.type === 'content' && item.content) {
                const contentData = item.content;
                if (contentData.type === 'text' && contentData.text) {
                  pre.textContent += contentData.text;
                }
              } else if (item.type === 'diff') {
                pre.textContent += 'Diff for: ' + item.path + '\\n';
                pre.textContent += item.newText || '';
              }
            }

            codeDiv.appendChild(pre);
            content.appendChild(codeDiv);
          }

          content.appendChild(infoDiv);
        }
      }

      div.appendChild(header);
      div.appendChild(content);
      return div;
    }

    // Render all messages
    for (const message of messages) {
      container.appendChild(renderMessage(message));
    }
  </script>
</body>

</html>
`;
