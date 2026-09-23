/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

// English translations for Qwen Code CLI
// The key serves as both the translation key and the default English text

export default {
  // ============================================================================
  // Help / UI Components
  // ============================================================================
  'Basics:': 'Basics:',
  'Add context': 'Add context',
  'Use {{symbol}} to specify files for context (e.g., {{example}}) to target specific files or folders.':
    'Use {{symbol}} to specify files for context (e.g., {{example}}) to target specific files or folders.',
  '@': '@',
  '@src/myFile.ts': '@src/myFile.ts',
  'Shell mode': 'Shell mode',
  'YOLO mode': 'YOLO mode',
  'plan mode': 'plan mode',
  'auto-accept edits': 'auto-accept edits',
  'Accepting edits': 'Accepting edits',
  '(shift + tab to cycle)': '(shift + tab to cycle)',
  'Execute shell commands via {{symbol}} (e.g., {{example1}}) or use natural language (e.g., {{example2}}).':
    'Execute shell commands via {{symbol}} (e.g., {{example1}}) or use natural language (e.g., {{example2}}).',
  '!': '!',
  '!npm run start': '!npm run start',
  'start server': 'start server',
  'Commands:': 'Commands:',
  'shell command': 'shell command',
  'Model Context Protocol command (from external servers)':
    'Model Context Protocol command (from external servers)',
  'Keyboard Shortcuts:': 'Keyboard Shortcuts:',
  'Toggle this help display': 'Toggle this help display',
  'Toggle shell mode': 'Toggle shell mode',
  'Open command menu': 'Open command menu',
  'Add file context': 'Add file context',
  'Accept suggestion / Autocomplete': 'Accept suggestion / Autocomplete',
  'Reverse search history': 'Reverse search history',
  'Press ? again to close': 'Press ? again to close',
  // Keyboard shortcuts panel descriptions
  'for shell mode': 'for shell mode',
  'for commands': 'for commands',
  'for file paths': 'for file paths',
  'to clear input': 'to clear input',
  'to cycle approvals': 'to cycle approvals',
  'to quit': 'to quit',
  'for newline': 'for newline',
  'to clear screen': 'to clear screen',
  // ============================================================================
  // Paste Placeholder
  // ============================================================================
  'input.paste.placeholder': '[Pasted Content {{charCount}} chars]',
  'input.paste.placeholder.numbered':
    '[Pasted Content {{charCount}} chars] #{{id}}',
  'to search history': 'to search history',
  'to paste images': 'to paste images',
  'for external editor': 'for external editor',
  'Jump through words in the input': 'Jump through words in the input',
  'Close dialogs, cancel requests, or quit application':
    'Close dialogs, cancel requests, or quit application',
  'New line': 'New line',
  'New line (Alt+Enter works for certain linux distros)':
    'New line (Alt+Enter works for certain linux distros)',
  'Clear the screen': 'Clear the screen',
  'Open input in external editor': 'Open input in external editor',
  'Send message': 'Send message',
  'Initializing...': 'Initializing...',
  'Connecting to MCP servers... ({{connected}}/{{total}})':
    'Connecting to MCP servers... ({{connected}}/{{total}})',
  'Type your message or @path/to/file': 'Type your message or @path/to/file',
  '? for shortcuts': '? for shortcuts',
  'Ctrl+C to exit': 'Ctrl+C to exit',
  '/bash for shell': '/bash for shell',
  "Press 'i' for INSERT mode and 'Esc' for NORMAL mode.":
    "Press 'i' for INSERT mode and 'Esc' for NORMAL mode.",
  'Cancel operation / Clear input (double press)':
    'Cancel operation / Clear input (double press)',
  'Cycle approval modes': 'Cycle approval modes',
  'Cycle through your prompt history': 'Cycle through your prompt history',
  'For a full list of shortcuts, see {{docPath}}':
    'For a full list of shortcuts, see {{docPath}}',
  'docs/keyboard-shortcuts.md': 'docs/keyboard-shortcuts.md',
  'for help on Copilot Shell': 'for help on Copilot Shell',
  'show version info': 'show version info',
  'submit a bug report': 'submit a bug report',
  'About Copilot Shell': 'About Copilot Shell',
  Status: 'Status',

  // ============================================================================
  // System Information Fields
  // ============================================================================
  'Copilot Shell': 'Copilot Shell',
  Runtime: 'Runtime',
  OS: 'OS',
  Auth: 'Auth',
  'CLI Version': 'CLI Version',
  'Git Commit': 'Git Commit',
  Model: 'Model',
  'OS Platform': 'OS Platform',
  'OS Arch': 'OS Arch',
  'OS Release': 'OS Release',
  'Node.js Version': 'Node.js Version',
  'NPM Version': 'NPM Version',
  'Session ID': 'Session ID',
  'Auth Method': 'Auth Method',
  'Base URL': 'Base URL',
  Proxy: 'Proxy',
  'Memory Usage': 'Memory Usage',
  'IDE Client': 'IDE Client',

  // ============================================================================
  // Commands - General
  // ============================================================================
  'Analyzes the project and creates a tailored COPILOT.md file.':
    'Analyzes the project and creates a tailored COPILOT.md file.',
  'list available Copilot Shell tools. Usage: /tools [desc]':
    'list available Copilot Shell tools. Usage: /tools [desc]',
  'Available Copilot Shell CLI tools:': 'Available Copilot Shell CLI tools:',
  'No tools available': 'No tools available',
  'View or change the approval mode for tool usage':
    'View or change the approval mode for tool usage',
  'Invalid approval mode "{{arg}}". Valid modes: {{modes}}':
    'Invalid approval mode "{{arg}}". Valid modes: {{modes}}',
  'Approval mode set to "{{mode}}"': 'Approval mode set to "{{mode}}"',
  'View or change the language setting': 'View or change the language setting',
  'change the theme': 'change the theme',
  'Select Theme': 'Select Theme',
  Preview: 'Preview',
  '(Use Enter to select, Tab to configure scope)':
    '(Use Enter to select, Tab to configure scope)',
  '(Use Enter to apply scope, Tab to go back)':
    '(Use Enter to apply scope, Tab to go back)',
  'Theme configuration unavailable due to NO_COLOR env variable.':
    'Theme configuration unavailable due to NO_COLOR env variable.',
  'Theme "{{themeName}}" not found.': 'Theme "{{themeName}}" not found.',
  'Theme "{{themeName}}" not found in selected scope.':
    'Theme "{{themeName}}" not found in selected scope.',
  'Clear conversation history and free up context':
    'Clear conversation history and free up context',
  'Compresses the context by replacing it with a summary.':
    'Compresses the context by replacing it with a summary.',
  'open full Copilot Shell documentation in your browser':
    'open full Copilot Shell documentation in your browser',
  'Configuration not available.': 'Configuration not available.',
  'change the auth method': 'change the auth method',
  'Copy the last result or code snippet to clipboard':
    'Copy the last result or code snippet to clipboard',

  // ============================================================================
  // Commands - Agents
  // ============================================================================
  'Manage subagents for specialized task delegation.':
    'Manage subagents for specialized task delegation.',
  'Manage existing subagents (view, edit, delete).':
    'Manage existing subagents (view, edit, delete).',
  'Create a new subagent with guided setup.':
    'Create a new subagent with guided setup.',

  // ============================================================================
  // Agents - Management Dialog
  // ============================================================================
  Agents: 'Agents',
  'Choose Action': 'Choose Action',
  'Edit {{name}}': 'Edit {{name}}',
  'Edit Tools: {{name}}': 'Edit Tools: {{name}}',
  'Edit Color: {{name}}': 'Edit Color: {{name}}',
  'Delete {{name}}': 'Delete {{name}}',
  'Unknown Step': 'Unknown Step',
  'Esc to close': 'Esc to close',
  'Enter to select, ↑↓ to navigate, Esc to close':
    'Enter to select, ↑↓ to navigate, Esc to close',
  'Esc to go back': 'Esc to go back',
  'Enter to confirm, Esc to cancel': 'Enter to confirm, Esc to cancel',
  'Enter to select, ↑↓ to navigate, Esc to go back':
    'Enter to select, ↑↓ to navigate, Esc to go back',
  'Invalid step: {{step}}': 'Invalid step: {{step}}',
  'No subagents found.': 'No subagents found.',
  "Use '/agents create' to create your first subagent.":
    "Use '/agents create' to create your first subagent.",
  '(built-in)': '(built-in)',
  '(overridden by project level agent)': '(overridden by project level agent)',
  'Project Level ({{path}})': 'Project Level ({{path}})',
  'User Level ({{path}})': 'User Level ({{path}})',
  'Built-in Agents': 'Built-in Agents',
  'Extension Agents': 'Extension Agents',
  'Using: {{count}} agents': 'Using: {{count}} agents',
  'View Agent': 'View Agent',
  'Edit Agent': 'Edit Agent',
  'Delete Agent': 'Delete Agent',
  Back: 'Back',
  'No agent selected': 'No agent selected',
  'File Path: ': 'File Path: ',
  'Tools: ': 'Tools: ',
  'Color: ': 'Color: ',
  'Description:': 'Description:',
  'System Prompt:': 'System Prompt:',
  'Open in editor': 'Open in editor',
  'Edit tools': 'Edit tools',
  'Edit color': 'Edit color',
  '❌ Error:': '❌ Error:',
  'Are you sure you want to delete agent "{{name}}"?':
    'Are you sure you want to delete agent "{{name}}"?',
  // ============================================================================
  // Agents - Creation Wizard
  // ============================================================================
  'Project Level (.copilot-shell/agents/)':
    'Project Level (.copilot-shell/agents/)',
  'User Level (~/.copilot-shell/agents/)':
    'User Level (~/.copilot-shell/agents/)',
  '✅ Subagent Created Successfully!': '✅ Subagent Created Successfully!',
  'Subagent "{{name}}" has been saved to {{level}} level.':
    'Subagent "{{name}}" has been saved to {{level}} level.',
  'Name: ': 'Name: ',
  'Location: ': 'Location: ',
  '❌ Error saving subagent:': '❌ Error saving subagent:',
  'Warnings:': 'Warnings:',
  'Name "{{name}}" already exists at {{level}} level - will overwrite existing subagent':
    'Name "{{name}}" already exists at {{level}} level - will overwrite existing subagent',
  'Name "{{name}}" exists at user level - project level will take precedence':
    'Name "{{name}}" exists at user level - project level will take precedence',
  'Name "{{name}}" exists at project level - existing subagent will take precedence':
    'Name "{{name}}" exists at project level - existing subagent will take precedence',
  'Description is over {{length}} characters':
    'Description is over {{length}} characters',
  'System prompt is over {{length}} characters':
    'System prompt is over {{length}} characters',
  // Agents - Creation Wizard Steps
  'Step {{n}}: Choose Location': 'Step {{n}}: Choose Location',
  'Step {{n}}: Choose Generation Method':
    'Step {{n}}: Choose Generation Method',
  'Generate with Copilot Shell (Recommended)':
    'Generate with Copilot Shell (Recommended)',
  'Manual Creation': 'Manual Creation',
  'Describe what this subagent should do and when it should be used. (Be comprehensive for best results)':
    'Describe what this subagent should do and when it should be used. (Be comprehensive for best results)',
  'e.g., Expert code reviewer that reviews code based on best practices...':
    'e.g., Expert code reviewer that reviews code based on best practices...',
  'Generating subagent configuration...':
    'Generating subagent configuration...',
  'Failed to generate subagent: {{error}}':
    'Failed to generate subagent: {{error}}',
  'Step {{n}}: Describe Your Subagent': 'Step {{n}}: Describe Your Subagent',
  'Step {{n}}: Enter Subagent Name': 'Step {{n}}: Enter Subagent Name',
  'Step {{n}}: Enter System Prompt': 'Step {{n}}: Enter System Prompt',
  'Step {{n}}: Enter Description': 'Step {{n}}: Enter Description',
  // Agents - Tool Selection
  'Step {{n}}: Select Tools': 'Step {{n}}: Select Tools',
  'All Tools (Default)': 'All Tools (Default)',
  'All Tools': 'All Tools',
  'Read-only Tools': 'Read-only Tools',
  'Read & Edit Tools': 'Read & Edit Tools',
  'Read & Edit & Execution Tools': 'Read & Edit & Execution Tools',
  'All tools selected, including MCP tools':
    'All tools selected, including MCP tools',
  'Selected tools:': 'Selected tools:',
  'Read-only tools:': 'Read-only tools:',
  'Edit tools:': 'Edit tools:',
  'Execution tools:': 'Execution tools:',
  'Step {{n}}: Choose Background Color': 'Step {{n}}: Choose Background Color',
  'Step {{n}}: Confirm and Save': 'Step {{n}}: Confirm and Save',
  // Agents - Navigation & Instructions
  'Esc to cancel': 'Esc to cancel',
  'Press Enter to save, e to save and edit, Esc to go back':
    'Press Enter to save, e to save and edit, Esc to go back',
  'Press Enter to continue, {{navigation}}Esc to {{action}}':
    'Press Enter to continue, {{navigation}}Esc to {{action}}',
  cancel: 'cancel',
  'go back': 'go back',
  '↑↓ to navigate, ': '↑↓ to navigate, ',
  'Enter a clear, unique name for this subagent.':
    'Enter a clear, unique name for this subagent.',
  'e.g., Code Reviewer': 'e.g., Code Reviewer',
  'Name cannot be empty.': 'Name cannot be empty.',
  "Write the system prompt that defines this subagent's behavior. Be comprehensive for best results.":
    "Write the system prompt that defines this subagent's behavior. Be comprehensive for best results.",
  'e.g., You are an expert code reviewer...':
    'e.g., You are an expert code reviewer...',
  'System prompt cannot be empty.': 'System prompt cannot be empty.',
  'Describe when and how this subagent should be used.':
    'Describe when and how this subagent should be used.',
  'e.g., Reviews code for best practices and potential bugs.':
    'e.g., Reviews code for best practices and potential bugs.',
  'Description cannot be empty.': 'Description cannot be empty.',
  'Failed to launch editor: {{error}}': 'Failed to launch editor: {{error}}',
  'Failed to save and edit subagent: {{error}}':
    'Failed to save and edit subagent: {{error}}',

  // ============================================================================
  // Commands - General (continued)
  // ============================================================================
  'View and edit Copilot Shell settings':
    'View and edit Copilot Shell settings',
  Settings: 'Settings',
  'To see changes, Copilot Shell must be restarted. Press r to exit and apply changes now.':
    'To see changes, Copilot Shell must be restarted. Press r to exit and apply changes now.',
  'The command "/{{command}}" is not supported in non-interactive mode.':
    'The command "/{{command}}" is not supported in non-interactive mode.',
  // ============================================================================
  // Settings Labels
  // ============================================================================
  'Vim Mode': 'Vim Mode',
  'Disable Auto Update': 'Disable Auto Update',
  'Attribution: commit': 'Attribution: commit',
  'Terminal Bell Notification': 'Terminal Bell Notification',
  'Enable Usage Statistics': 'Enable Usage Statistics',
  Theme: 'Theme',
  'Preferred Editor': 'Preferred Editor',
  'Auto-connect to IDE': 'Auto-connect to IDE',
  'Enable Prompt Completion': 'Enable Prompt Completion',
  'Debug Keystroke Logging': 'Debug Keystroke Logging',
  'Language: UI': 'Language: UI',
  'Language: Model': 'Language: Model',
  'Output Format': 'Output Format',
  'Hide Window Title': 'Hide Window Title',
  'Show Status in Title': 'Show Status in Title',
  'Hide Tips': 'Hide Tips',
  'Hide Feature Tip Banner': 'Hide Feature Tip Banner',
  'Hide the one-time feature tip banner shown on first launch':
    'Hide the one-time feature tip banner shown on first launch',
  'Show Line Numbers in Code': 'Show Line Numbers in Code',
  'Show Citations': 'Show Citations',
  'Custom Witty Phrases': 'Custom Witty Phrases',
  'Show Welcome Back Dialog': 'Show Welcome Back Dialog',
  'Enable User Feedback': 'Enable User Feedback',
  'How is Qwen doing this session? (optional)':
    'How is Qwen doing this session? (optional)',
  Bad: 'Bad',
  Fine: 'Fine',
  Good: 'Good',
  Dismiss: 'Dismiss',
  'Not Sure Yet': 'Not Sure Yet',
  'Any other key': 'Any other key',
  'Disable Loading Phrases': 'Disable Loading Phrases',
  'Screen Reader Mode': 'Screen Reader Mode',
  'IDE Mode': 'IDE Mode',
  'Max Session Turns': 'Max Session Turns',
  'Skip Next Speaker Check': 'Skip Next Speaker Check',
  'Skip Loop Detection': 'Skip Loop Detection',
  'Skip Startup Context': 'Skip Startup Context',
  'Enable OpenAI Logging': 'Enable OpenAI Logging',
  'OpenAI Logging Directory': 'OpenAI Logging Directory',
  Timeout: 'Timeout',
  'Max Retries': 'Max Retries',
  'Disable Cache Control': 'Disable Cache Control',
  'Memory Discovery Max Dirs': 'Memory Discovery Max Dirs',
  'Load Memory From Include Directories':
    'Load Memory From Include Directories',
  'Respect .gitignore': 'Respect .gitignore',
  'Respect .copilotignore': 'Respect .copilotignore',
  'Enable Recursive File Search': 'Enable Recursive File Search',
  'Disable Fuzzy Search': 'Disable Fuzzy Search',
  'Interactive Shell (PTY)': 'Interactive Shell (PTY)',
  'Show Color': 'Show Color',
  'Auto Accept': 'Auto Accept',
  'Use Ripgrep': 'Use Ripgrep',
  'Use Builtin Ripgrep': 'Use Builtin Ripgrep',
  'Enable Tool Output Truncation': 'Enable Tool Output Truncation',
  'Tool Output Truncation Threshold': 'Tool Output Truncation Threshold',
  'Tool Output Truncation Lines': 'Tool Output Truncation Lines',
  'Folder Trust': 'Folder Trust',
  'Vision Model Preview': 'Vision Model Preview',
  'Tool Schema Compliance': 'Tool Schema Compliance',
  // Settings enum options
  'Auto (detect from system)': 'Auto (detect from system)',
  Text: 'Text',
  JSON: 'JSON',
  Plan: 'Plan',
  Default: 'Default',
  'Auto Edit': 'Auto Edit',
  YOLO: 'YOLO',
  'toggle vim mode on/off': 'toggle vim mode on/off',
  'check session stats. Usage: /stats [model|tools]':
    'check session stats. Usage: /stats [model|tools]',
  'Show model-specific usage statistics.':
    'Show model-specific usage statistics.',
  'Show tool-specific usage statistics.':
    'Show tool-specific usage statistics.',
  'exit the cli': 'exit the cli',
  'list configured MCP servers and tools, or authenticate with OAuth-enabled servers':
    'list configured MCP servers and tools, or authenticate with OAuth-enabled servers',
  'Manage workspace directories': 'Manage workspace directories',
  'Add directories to the workspace. Use comma to separate multiple paths':
    'Add directories to the workspace. Use comma to separate multiple paths',
  'Show all directories in the workspace':
    'Show all directories in the workspace',
  'Switch the working directory for the current session':
    'Switch the working directory for the current session',
  'Please provide a path to switch to. Usage: /dir cd <path>':
    'Please provide a path to switch to. Usage: /dir cd <path>',
  'Directory "{{path}}" does not exist.':
    'Directory "{{path}}" does not exist.',
  'Failed to change directory to "{{path}}": {{error}}':
    'Failed to change directory to "{{path}}": {{error}}',
  'Switched working directory to: {{path}}':
    'Switched working directory to: {{path}}',
  'You are running Copilot Shell in your home directory. It is recommended to run in a project-specific directory. Use "/dir cd <path>" to switch to a project directory.':
    'You are running Copilot Shell in your home directory. It is recommended to run in a project-specific directory. Use "/dir cd <path>" to switch to a project directory.',
  'Warning: You are running Copilot Shell in the root directory. Your entire folder structure will be used for context. It is strongly recommended to run in a project-specific directory.':
    'Warning: You are running Copilot Shell in the root directory. Your entire folder structure will be used for context. It is strongly recommended to run in a project-specific directory.',
  'Could not verify the current directory due to a file system error.':
    'Could not verify the current directory due to a file system error.',
  'Ripgrep not available: Please install ripgrep globally to enable faster file content search. Falling back to built-in grep.':
    'Ripgrep not available: Please install ripgrep globally to enable faster file content search. Falling back to built-in grep.',
  'Ripgrep not available: {{message}}. Falling back to built-in grep.':
    'Ripgrep not available: {{message}}. Falling back to built-in grep.',
  'Qwen OAuth service was discontinued on April 15, 2026. Please run /auth to select a supported authentication method.':
    'Qwen OAuth service was discontinued on April 15, 2026. Please run /auth to select a supported authentication method.',
  'set external editor preference': 'set external editor preference',
  'Select Editor': 'Select Editor',
  'Editor Preference': 'Editor Preference',
  'These editors are currently supported.':
    'These editors are currently supported.',
  'Your preferred editor is:': 'Your preferred editor is:',
  'Manage extensions': 'Manage extensions',
  'List active extensions': 'List active extensions',
  'Update extensions. Usage: update <extension-names>|--all':
    'Update extensions. Usage: update <extension-names>|--all',
  'Disable an extension': 'Disable an extension',
  'Enable an extension': 'Enable an extension',
  'Install an extension from a git repo or local path':
    'Install an extension from a git repo or local path',
  'Uninstall an extension': 'Uninstall an extension',
  'No extensions installed.': 'No extensions installed.',
  'Usage: /extensions update <extension-names>|--all':
    'Usage: /extensions update <extension-names>|--all',
  'Extension "{{name}}" not found.': 'Extension "{{name}}" not found.',
  'No extensions to update.': 'No extensions to update.',
  'Usage: /extensions install <source>': 'Usage: /extensions install <source>',
  'Installing extension from "{{source}}"...':
    'Installing extension from "{{source}}"...',
  'Extension "{{name}}" installed successfully.':
    'Extension "{{name}}" installed successfully.',
  'Failed to install extension from "{{source}}": {{error}}':
    'Failed to install extension from "{{source}}": {{error}}',
  'Usage: /extensions uninstall <extension-name>':
    'Usage: /extensions uninstall <extension-name>',
  'Uninstalling extension "{{name}}"...':
    'Uninstalling extension "{{name}}"...',
  'Extension "{{name}}" uninstalled successfully.':
    'Extension "{{name}}" uninstalled successfully.',
  'Failed to uninstall extension "{{name}}": {{error}}':
    'Failed to uninstall extension "{{name}}": {{error}}',
  'Usage: /extensions {{command}} <extension> [--scope=<user|workspace>]':
    'Usage: /extensions {{command}} <extension> [--scope=<user|workspace>]',
  'Unsupported scope "{{scope}}", should be one of "user" or "workspace"':
    'Unsupported scope "{{scope}}", should be one of "user" or "workspace"',
  'Extension "{{name}}" disabled for scope "{{scope}}"':
    'Extension "{{name}}" disabled for scope "{{scope}}"',
  'Extension "{{name}}" enabled for scope "{{scope}}"':
    'Extension "{{name}}" enabled for scope "{{scope}}"',
  'Do you want to continue? [Y/n]: ': 'Do you want to continue? [Y/n]: ',
  'Do you want to continue?': 'Do you want to continue?',
  'Installing extension "{{name}}".': 'Installing extension "{{name}}".',
  '**Extensions may introduce unexpected behavior. Ensure you have investigated the extension source and trust the author.**':
    '**Extensions may introduce unexpected behavior. Ensure you have investigated the extension source and trust the author.**',
  'This extension will run the following MCP servers:':
    'This extension will run the following MCP servers:',
  local: 'local',
  remote: 'remote',
  'This extension will add the following commands: {{commands}}.':
    'This extension will add the following commands: {{commands}}.',
  'This extension will append info to your COPILOT.md context using {{fileName}}':
    'This extension will append info to your COPILOT.md context using {{fileName}}',
  'This extension will exclude the following core tools: {{tools}}':
    'This extension will exclude the following core tools: {{tools}}',
  'This extension will install the following skills:':
    'This extension will install the following skills:',
  'This extension will install the following subagents:':
    'This extension will install the following subagents:',
  'Installation cancelled for "{{name}}".':
    'Installation cancelled for "{{name}}".',
  '--ref and --auto-update are not applicable for marketplace extensions.':
    '--ref and --auto-update are not applicable for marketplace extensions.',
  'Extension "{{name}}" installed successfully and enabled.':
    'Extension "{{name}}" installed successfully and enabled.',
  'Installs an extension from a git repository URL, local path, or claude marketplace (marketplace-url:plugin-name).':
    'Installs an extension from a git repository URL, local path, or claude marketplace (marketplace-url:plugin-name).',
  'The github URL, local path, or marketplace source (marketplace-url:plugin-name) of the extension to install.':
    'The github URL, local path, or marketplace source (marketplace-url:plugin-name) of the extension to install.',
  'The git ref to install from.': 'The git ref to install from.',
  'Enable auto-update for this extension.':
    'Enable auto-update for this extension.',
  'Enable pre-release versions for this extension.':
    'Enable pre-release versions for this extension.',
  'Acknowledge the security risks of installing an extension and skip the confirmation prompt.':
    'Acknowledge the security risks of installing an extension and skip the confirmation prompt.',
  'The source argument must be provided.':
    'The source argument must be provided.',
  'Extension "{{name}}" successfully uninstalled.':
    'Extension "{{name}}" successfully uninstalled.',
  'Uninstalls an extension.': 'Uninstalls an extension.',
  'The name or source path of the extension to uninstall.':
    'The name or source path of the extension to uninstall.',
  'Please include the name of the extension to uninstall as a positional argument.':
    'Please include the name of the extension to uninstall as a positional argument.',
  'Enables an extension.': 'Enables an extension.',
  'The name of the extension to enable.':
    'The name of the extension to enable.',
  'The scope to enable the extenison in. If not set, will be enabled in all scopes.':
    'The scope to enable the extenison in. If not set, will be enabled in all scopes.',
  'Extension "{{name}}" successfully enabled for scope "{{scope}}".':
    'Extension "{{name}}" successfully enabled for scope "{{scope}}".',
  'Extension "{{name}}" successfully enabled in all scopes.':
    'Extension "{{name}}" successfully enabled in all scopes.',
  'Invalid scope: {{scope}}. Please use one of {{scopes}}.':
    'Invalid scope: {{scope}}. Please use one of {{scopes}}.',
  'Disables an extension.': 'Disables an extension.',
  'The name of the extension to disable.':
    'The name of the extension to disable.',
  'The scope to disable the extenison in.':
    'The scope to disable the extenison in.',
  'Extension "{{name}}" successfully disabled for scope "{{scope}}".':
    'Extension "{{name}}" successfully disabled for scope "{{scope}}".',
  'Extension "{{name}}" successfully updated: {{oldVersion}} → {{newVersion}}.':
    'Extension "{{name}}" successfully updated: {{oldVersion}} → {{newVersion}}.',
  'Unable to install extension "{{name}}" due to missing install metadata':
    'Unable to install extension "{{name}}" due to missing install metadata',
  'Extension "{{name}}" is already up to date.':
    'Extension "{{name}}" is already up to date.',
  'Updates all extensions or a named extension to the latest version.':
    'Updates all extensions or a named extension to the latest version.',
  'Update all extensions.': 'Update all extensions.',
  'Either an extension name or --all must be provided':
    'Either an extension name or --all must be provided',
  'Lists installed extensions.': 'Lists installed extensions.',
  'Path:': 'Path:',
  'Source:': 'Source:',
  'Type:': 'Type:',
  'Ref:': 'Ref:',
  'Release tag:': 'Release tag:',
  'Enabled (User):': 'Enabled (User):',
  'Enabled (Workspace):': 'Enabled (Workspace):',
  'Context files:': 'Context files:',
  'Skills:': 'Skills:',
  'Agents:': 'Agents:',
  'MCP servers:': 'MCP servers:',
  'Link extension failed to install.': 'Link extension failed to install.',
  'Extension "{{name}}" linked successfully and enabled.':
    'Extension "{{name}}" linked successfully and enabled.',
  'Links an extension from a local path. Updates made to the local path will always be reflected.':
    'Links an extension from a local path. Updates made to the local path will always be reflected.',
  'The name of the extension to link.': 'The name of the extension to link.',
  'Set a specific setting for an extension.':
    'Set a specific setting for an extension.',
  'Name of the extension to configure.': 'Name of the extension to configure.',
  'The setting to configure (name or env var).':
    'The setting to configure (name or env var).',
  'The scope to set the setting in.': 'The scope to set the setting in.',
  'List all settings for an extension.': 'List all settings for an extension.',
  'Name of the extension.': 'Name of the extension.',
  'Extension "{{name}}" has no settings to configure.':
    'Extension "{{name}}" has no settings to configure.',
  'Settings for "{{name}}":': 'Settings for "{{name}}":',
  '(workspace)': '(workspace)',
  '(user)': '(user)',
  '[not set]': '[not set]',
  '[value stored in keychain]': '[value stored in keychain]',
  'Value:': 'Value:',
  'Manage extension settings.': 'Manage extension settings.',
  'You need to specify a command (set or list).':
    'You need to specify a command (set or list).',
  // ============================================================================
  // Plugin Choice / Marketplace
  // ============================================================================
  'No plugins available in this marketplace.':
    'No plugins available in this marketplace.',
  'Select a plugin to install from marketplace "{{name}}":':
    'Select a plugin to install from marketplace "{{name}}":',
  'Plugin selection cancelled.': 'Plugin selection cancelled.',
  'Select a plugin from "{{name}}"': 'Select a plugin from "{{name}}"',
  'Use ↑↓ or j/k to navigate, Enter to select, Escape to cancel':
    'Use ↑↓ or j/k to navigate, Enter to select, Escape to cancel',
  '{{count}} more above': '{{count}} more above',
  '{{count}} more below': '{{count}} more below',
  'manage IDE integration': 'manage IDE integration',
  'check status of IDE integration': 'check status of IDE integration',
  'install required IDE companion for {{ideName}}':
    'install required IDE companion for {{ideName}}',
  'enable IDE integration': 'enable IDE integration',
  'disable IDE integration': 'disable IDE integration',
  'IDE integration is not supported in your current environment. To use this feature, run Copilot Shell in one of these supported IDEs: VS Code or VS Code forks.':
    'IDE integration is not supported in your current environment. To use this feature, run Copilot Shell in one of these supported IDEs: VS Code or VS Code forks.',
  'Set up GitHub Actions': 'Set up GitHub Actions',
  'Configure terminal keybindings for multiline input (VS Code, Cursor, Windsurf, Trae)':
    'Configure terminal keybindings for multiline input (VS Code, Cursor, Windsurf, Trae)',
  'Please restart your terminal for the changes to take effect.':
    'Please restart your terminal for the changes to take effect.',
  'Failed to configure terminal: {{error}}':
    'Failed to configure terminal: {{error}}',
  'Could not determine {{terminalName}} config path on Windows: APPDATA environment variable is not set.':
    'Could not determine {{terminalName}} config path on Windows: APPDATA environment variable is not set.',
  '{{terminalName}} keybindings.json exists but is not a valid JSON array. Please fix the file manually or delete it to allow automatic configuration.':
    '{{terminalName}} keybindings.json exists but is not a valid JSON array. Please fix the file manually or delete it to allow automatic configuration.',
  'File: {{file}}': 'File: {{file}}',
  'Failed to parse {{terminalName}} keybindings.json. The file contains invalid JSON. Please fix the file manually or delete it to allow automatic configuration.':
    'Failed to parse {{terminalName}} keybindings.json. The file contains invalid JSON. Please fix the file manually or delete it to allow automatic configuration.',
  'Error: {{error}}': 'Error: {{error}}',
  'Shift+Enter binding already exists': 'Shift+Enter binding already exists',
  'Ctrl+Enter binding already exists': 'Ctrl+Enter binding already exists',
  'Existing keybindings detected. Will not modify to avoid conflicts.':
    'Existing keybindings detected. Will not modify to avoid conflicts.',
  'Please check and modify manually if needed: {{file}}':
    'Please check and modify manually if needed: {{file}}',
  'Added Shift+Enter and Ctrl+Enter keybindings to {{terminalName}}.':
    'Added Shift+Enter and Ctrl+Enter keybindings to {{terminalName}}.',
  'Modified: {{file}}': 'Modified: {{file}}',
  '{{terminalName}} keybindings already configured.':
    '{{terminalName}} keybindings already configured.',
  'Failed to configure {{terminalName}}.':
    'Failed to configure {{terminalName}}.',
  'Your terminal is already configured for an optimal experience with multiline input (Shift+Enter and Ctrl+Enter).':
    'Your terminal is already configured for an optimal experience with multiline input (Shift+Enter and Ctrl+Enter).',
  'Could not detect terminal type. Supported terminals: VS Code, Cursor, Windsurf, and Trae.':
    'Could not detect terminal type. Supported terminals: VS Code, Cursor, Windsurf, and Trae.',
  'Terminal "{{terminal}}" is not supported yet.':
    'Terminal "{{terminal}}" is not supported yet.',

  // ============================================================================
  // Commands - Language
  // ============================================================================
  'Invalid language. Available: {{options}}':
    'Invalid language. Available: {{options}}',
  'Language subcommands do not accept additional arguments.':
    'Language subcommands do not accept additional arguments.',
  'Current UI language: {{lang}}': 'Current UI language: {{lang}}',
  'Current LLM output language: {{lang}}':
    'Current LLM output language: {{lang}}',
  'LLM output language not set': 'LLM output language not set',
  'Set UI language': 'Set UI language',
  'Set LLM output language': 'Set LLM output language',
  'Usage: /language ui [{{options}}]': 'Usage: /language ui [{{options}}]',
  'Usage: /language output <language>': 'Usage: /language output <language>',
  'Example: /language output 中文': 'Example: /language output 中文',
  'Example: /language output English': 'Example: /language output English',
  'Example: /language output 日本語': 'Example: /language output 日本語',
  'Example: /language output Português': 'Example: /language output Português',
  'UI language changed to {{lang}}': 'UI language changed to {{lang}}',
  'LLM output language set to {{lang}}': 'LLM output language set to {{lang}}',
  'LLM output language rule file generated at {{path}}':
    'LLM output language rule file generated at {{path}}',
  'Please restart the application for the changes to take effect.':
    'Please restart the application for the changes to take effect.',
  'Failed to generate LLM output language rule file: {{error}}':
    'Failed to generate LLM output language rule file: {{error}}',
  'Invalid command. Available subcommands:':
    'Invalid command. Available subcommands:',
  'Available subcommands:': 'Available subcommands:',
  'To request additional UI language packs, please open an issue on GitHub.':
    'To request additional UI language packs, please open an issue on GitHub.',
  'Available options:': 'Available options:',
  'Set UI language to {{name}}': 'Set UI language to {{name}}',

  // ============================================================================
  // Commands - Approval Mode
  // ============================================================================
  'Tool Approval Mode': 'Tool Approval Mode',
  'Current approval mode: {{mode}}': 'Current approval mode: {{mode}}',
  'Available approval modes:': 'Available approval modes:',
  'Approval mode changed to: {{mode}}': 'Approval mode changed to: {{mode}}',
  'Approval mode changed to: {{mode}} (saved to {{scope}} settings{{location}})':
    'Approval mode changed to: {{mode}} (saved to {{scope}} settings{{location}})',
  'Usage: /approval-mode <mode> [--session|--user|--project]':
    'Usage: /approval-mode <mode> [--session|--user|--project]',

  'Scope subcommands do not accept additional arguments.':
    'Scope subcommands do not accept additional arguments.',
  'Plan mode - Analyze only, do not modify files or execute commands':
    'Plan mode - Analyze only, do not modify files or execute commands',
  'Default mode - Require approval for file edits or shell commands':
    'Default mode - Require approval for file edits or shell commands',
  'Auto-edit mode - Automatically approve file edits':
    'Auto-edit mode - Automatically approve file edits',
  'YOLO mode - Automatically approve all tools':
    'YOLO mode - Automatically approve all tools',
  '{{mode}} mode': '{{mode}} mode',
  'Settings service is not available; unable to persist the approval mode.':
    'Settings service is not available; unable to persist the approval mode.',
  'Failed to save approval mode: {{error}}':
    'Failed to save approval mode: {{error}}',
  'Failed to change approval mode: {{error}}':
    'Failed to change approval mode: {{error}}',
  'Apply to current session only (temporary)':
    'Apply to current session only (temporary)',
  'Persist for this project/workspace': 'Persist for this project/workspace',
  'Persist for this user on this machine':
    'Persist for this user on this machine',
  'Analyze only, do not modify files or execute commands':
    'Analyze only, do not modify files or execute commands',
  'Require approval for file edits or shell commands':
    'Require approval for file edits or shell commands',
  'Automatically approve file edits': 'Automatically approve file edits',
  'Automatically approve all tools': 'Automatically approve all tools',
  'Workspace approval mode exists and takes priority. User-level change will have no effect.':
    'Workspace approval mode exists and takes priority. User-level change will have no effect.',
  'Apply To': 'Apply To',
  'User Settings': 'User Settings',
  'Workspace Settings': 'Workspace Settings',

  // ============================================================================
  // Commands - Memory
  // ============================================================================
  'Commands for interacting with memory.':
    'Commands for interacting with memory.',
  'Show the current memory contents.': 'Show the current memory contents.',
  'Show project-level memory contents.': 'Show project-level memory contents.',
  'Show global memory contents.': 'Show global memory contents.',
  'Add content to project-level memory.':
    'Add content to project-level memory.',
  'Add content to global memory.': 'Add content to global memory.',
  'Refresh the memory from the source.': 'Refresh the memory from the source.',
  'Usage: /memory add --project <text to remember>':
    'Usage: /memory add --project <text to remember>',
  'Usage: /memory add --global <text to remember>':
    'Usage: /memory add --global <text to remember>',
  'Attempting to save to project memory: "{{text}}"':
    'Attempting to save to project memory: "{{text}}"',
  'Attempting to save to global memory: "{{text}}"':
    'Attempting to save to global memory: "{{text}}"',
  'Current memory content from {{count}} file(s):':
    'Current memory content from {{count}} file(s):',
  'Memory is currently empty.': 'Memory is currently empty.',
  'Project memory file not found or is currently empty.':
    'Project memory file not found or is currently empty.',
  'Global memory file not found or is currently empty.':
    'Global memory file not found or is currently empty.',
  'Global memory is currently empty.': 'Global memory is currently empty.',
  'Global memory content:\n\n---\n{{content}}\n---':
    'Global memory content:\n\n---\n{{content}}\n---',
  'Project memory content from {{path}}:\n\n---\n{{content}}\n---':
    'Project memory content from {{path}}:\n\n---\n{{content}}\n---',
  'Project memory is currently empty.': 'Project memory is currently empty.',
  'Refreshing memory from source files...':
    'Refreshing memory from source files...',
  'Add content to the memory. Use --global for global memory or --project for project memory.':
    'Add content to the memory. Use --global for global memory or --project for project memory.',
  'Usage: /memory add [--global|--project] <text to remember>':
    'Usage: /memory add [--global|--project] <text to remember>',
  'Attempting to save to memory {{scope}}: "{{fact}}"':
    'Attempting to save to memory {{scope}}: "{{fact}}"',

  // ============================================================================
  // Commands - MCP
  // ============================================================================
  'Authenticate with an OAuth-enabled MCP server':
    'Authenticate with an OAuth-enabled MCP server',
  'List configured MCP servers and tools':
    'List configured MCP servers and tools',
  'Restarts MCP servers.': 'Restarts MCP servers.',
  'Config not loaded.': 'Config not loaded.',
  'Could not retrieve tool registry.': 'Could not retrieve tool registry.',
  'No MCP servers configured with OAuth authentication.':
    'No MCP servers configured with OAuth authentication.',
  'MCP servers with OAuth authentication:':
    'MCP servers with OAuth authentication:',
  'Use /mcp auth <server-name> to authenticate.':
    'Use /mcp auth <server-name> to authenticate.',
  "MCP server '{{name}}' not found.": "MCP server '{{name}}' not found.",
  "Successfully authenticated and refreshed tools for '{{name}}'.":
    "Successfully authenticated and refreshed tools for '{{name}}'.",
  "Failed to authenticate with MCP server '{{name}}': {{error}}":
    "Failed to authenticate with MCP server '{{name}}': {{error}}",
  "Re-discovering tools from '{{name}}'...":
    "Re-discovering tools from '{{name}}'...",

  // ============================================================================
  // Commands - Chat
  // ============================================================================
  'Manage conversation history.': 'Manage conversation history.',
  'List saved conversation checkpoints': 'List saved conversation checkpoints',
  'No saved conversation checkpoints found.':
    'No saved conversation checkpoints found.',
  'List of saved conversations:': 'List of saved conversations:',
  'Note: Newest last, oldest first': 'Note: Newest last, oldest first',
  'Save the current conversation as a checkpoint. Usage: /chat save <tag>':
    'Save the current conversation as a checkpoint. Usage: /chat save <tag>',
  'Missing tag. Usage: /chat save <tag>':
    'Missing tag. Usage: /chat save <tag>',
  'Delete a conversation checkpoint. Usage: /chat delete <tag>':
    'Delete a conversation checkpoint. Usage: /chat delete <tag>',
  'Missing tag. Usage: /chat delete <tag>':
    'Missing tag. Usage: /chat delete <tag>',
  "Conversation checkpoint '{{tag}}' has been deleted.":
    "Conversation checkpoint '{{tag}}' has been deleted.",
  "Error: No checkpoint found with tag '{{tag}}'.":
    "Error: No checkpoint found with tag '{{tag}}'.",
  'Resume a conversation from a checkpoint. Usage: /chat resume <tag>':
    'Resume a conversation from a checkpoint. Usage: /chat resume <tag>',
  'Missing tag. Usage: /chat resume <tag>':
    'Missing tag. Usage: /chat resume <tag>',
  'No saved checkpoint found with tag: {{tag}}.':
    'No saved checkpoint found with tag: {{tag}}.',
  'A checkpoint with the tag {{tag}} already exists. Do you want to overwrite it?':
    'A checkpoint with the tag {{tag}} already exists. Do you want to overwrite it?',
  'No chat client available to save conversation.':
    'No chat client available to save conversation.',
  'Conversation checkpoint saved with tag: {{tag}}.':
    'Conversation checkpoint saved with tag: {{tag}}.',
  'No conversation found to save.': 'No conversation found to save.',
  'No chat client available to share conversation.':
    'No chat client available to share conversation.',
  'Invalid file format. Only .md and .json are supported.':
    'Invalid file format. Only .md and .json are supported.',
  'Error sharing conversation: {{error}}':
    'Error sharing conversation: {{error}}',
  'Conversation shared to {{filePath}}': 'Conversation shared to {{filePath}}',
  'No conversation found to share.': 'No conversation found to share.',
  'Share the current conversation to a markdown or json file. Usage: /chat share <file>':
    'Share the current conversation to a markdown or json file. Usage: /chat share <file>',

  // ============================================================================
  // Commands - Summary
  // ============================================================================
  'Generate a project summary and save it to .copilot-shell/PROJECT_SUMMARY.md':
    'Generate a project summary and save it to .copilot-shell/PROJECT_SUMMARY.md',
  'No chat client available to generate summary.':
    'No chat client available to generate summary.',
  'Already generating summary, wait for previous request to complete':
    'Already generating summary, wait for previous request to complete',
  'No conversation found to summarize.': 'No conversation found to summarize.',
  'Failed to generate project context summary: {{error}}':
    'Failed to generate project context summary: {{error}}',
  'Saved project summary to {{filePathForDisplay}}.':
    'Saved project summary to {{filePathForDisplay}}.',
  'Saving project summary...': 'Saving project summary...',
  'Generating project summary...': 'Generating project summary...',
  'Processing summary...': 'Processing summary...',
  'Project summary generated and saved successfully!':
    'Project summary generated and saved successfully!',
  '{{baseMessage}} Saved to: {{filePath}}':
    '{{baseMessage}} Saved to: {{filePath}}',
  'Failed to generate summary - no text content received from LLM response':
    'Failed to generate summary - no text content received from LLM response',

  // ============================================================================
  // Commands - Model
  // ============================================================================
  'Switch the model for this session': 'Switch the model for this session',
  'Content generator configuration not available.':
    'Content generator configuration not available.',
  'Authentication type not available.': 'Authentication type not available.',
  'No models available for the current authentication type ({{authType}}).':
    'No models available for the current authentication type ({{authType}}).',

  // ============================================================================
  // Commands - Resume
  // ============================================================================
  'Resume a previous session': 'Resume a previous session',

  // Commands - Rename
  // ============================================================================
  'Rename the current session': 'Rename the current session',
  'No active session available.': 'No active session available.',
  'Session name set to: {{name}}': 'Session name set to: {{name}}',

  // Session Picker - Rename & Preview
  'Press Ctrl+R to rename · Ctrl+V to preview':
    'Press Ctrl+R to rename · Ctrl+V to preview',
  'Enter new session name:': 'Enter new session name:',
  'Session renamed.': 'Session renamed.',
  'Failed to rename session.': 'Failed to rename session.',
  '(empty prompt)': '(empty prompt)',
  'to rename': 'to rename',
  'to preview': 'to preview',
  'Loading preview...': 'Loading preview...',
  'No messages to preview.': 'No messages to preview.',

  // ============================================================================
  // Commands - Clear
  // ============================================================================
  'Starting a new session, resetting chat, and clearing terminal.':
    'Starting a new session, resetting chat, and clearing terminal.',
  'Starting a new session and clearing.':
    'Starting a new session and clearing.',

  // ============================================================================
  // Commands - Compress
  // ============================================================================
  'Already compressing, wait for previous request to complete':
    'Already compressing, wait for previous request to complete',
  'Failed to compress chat history.': 'Failed to compress chat history.',
  'Failed to compress chat history: {{error}}':
    'Failed to compress chat history: {{error}}',
  'Compressing chat history': 'Compressing chat history',
  'Chat history compressed from {{originalTokens}} to {{newTokens}} tokens.':
    'Chat history compressed from {{originalTokens}} to {{newTokens}} tokens.',
  'Compression was not beneficial for this history size.':
    'Compression was not beneficial for this history size.',
  'Chat history compression did not reduce size. This may indicate issues with the compression prompt.':
    'Chat history compression did not reduce size. This may indicate issues with the compression prompt.',
  'Could not compress chat history due to a token counting error.':
    'Could not compress chat history due to a token counting error.',
  'Chat history is already compressed.': 'Chat history is already compressed.',

  // ============================================================================
  // Commands - Directory
  // ============================================================================
  'Configuration is not available.': 'Configuration is not available.',
  'Please provide at least one path to add.':
    'Please provide at least one path to add.',
  "Error adding '{{path}}': {{error}}": "Error adding '{{path}}': {{error}}",
  'Successfully added COPILOT.md files from the following directories if there are:\n- {{directories}}':
    'Successfully added COPILOT.md files from the following directories if there are:\n- {{directories}}',
  'Error refreshing memory: {{error}}': 'Error refreshing memory: {{error}}',
  'Successfully added directories:\n- {{directories}}':
    'Successfully added directories:\n- {{directories}}',
  'Current workspace directories:\n{{directories}}':
    'Current workspace directories:\n{{directories}}',

  // ============================================================================
  // Commands - Docs
  // ============================================================================
  'Please open the following URL in your browser to view the documentation:\n{{url}}':
    'Please open the following URL in your browser to view the documentation:\n{{url}}',
  'Opening documentation in your browser: {{url}}':
    'Opening documentation in your browser: {{url}}',

  // ============================================================================
  // Dialogs - Tool Confirmation
  // ============================================================================
  'Do you want to proceed?': 'Do you want to proceed?',
  'Yes, allow once': 'Yes, allow once',
  'Allow always': 'Allow always',
  No: 'No',
  'No (esc)': 'No (esc)',
  'Press Esc to interrupt': 'Press Esc to interrupt',
  'Yes, allow always for this session': 'Yes, allow always for this session',
  'Modify in progress:': 'Modify in progress:',
  'Save and close external editor to continue':
    'Save and close external editor to continue',
  'Apply this change?': 'Apply this change?',
  'Yes, allow always': 'Yes, allow always',
  'Modify with external editor': 'Modify with external editor',
  'No, suggest changes (esc)': 'No, suggest changes (esc)',
  'Do you want to execute this command?':
    'Do you want to execute this command?',
  'Yes, allow always ...': 'Yes, allow always ...',
  'Yes, and auto-accept edits': 'Yes, and auto-accept edits',
  'Yes, and manually approve edits': 'Yes, and manually approve edits',
  'No, keep planning (esc)': 'No, keep planning (esc)',
  'URLs to fetch:': 'URLs to fetch:',
  'MCP Server: {{server}}': 'MCP Server: {{server}}',
  'Tool: {{tool}}': 'Tool: {{tool}}',
  'Allow execution of MCP tool "{{tool}}" from server "{{server}}"?':
    'Allow execution of MCP tool "{{tool}}" from server "{{server}}"?',
  'Yes, always allow tool "{{tool}}" from server "{{server}}"':
    'Yes, always allow tool "{{tool}}" from server "{{server}}"',
  'Yes, always allow all tools from server "{{server}}"':
    'Yes, always allow all tools from server "{{server}}"',

  // ============================================================================
  // Dialogs - Shell Confirmation
  // ============================================================================
  'Shell Command Execution': 'Shell Command Execution',
  'A custom command wants to run the following shell commands:':
    'A custom command wants to run the following shell commands:',

  // ============================================================================
  // Dialogs - Pro Quota
  // ============================================================================
  'Pro quota limit reached for {{model}}.':
    'Pro quota limit reached for {{model}}.',
  'Change auth (executes the /auth command)':
    'Change auth (executes the /auth command)',
  'Continue with {{model}}': 'Continue with {{model}}',

  // ============================================================================
  // Dialogs - Welcome Back
  // ============================================================================
  'Current Plan:': 'Current Plan:',
  'Progress: {{done}}/{{total}} tasks completed':
    'Progress: {{done}}/{{total}} tasks completed',
  ', {{inProgress}} in progress': ', {{inProgress}} in progress',
  'Pending Tasks:': 'Pending Tasks:',
  'What would you like to do?': 'What would you like to do?',
  'Choose how to proceed with your session:':
    'Choose how to proceed with your session:',
  'Start new chat session': 'Start new chat session',
  'Continue previous conversation': 'Continue previous conversation',
  '👋 Welcome back! (Last updated: {{timeAgo}})':
    '👋 Welcome back! (Last updated: {{timeAgo}})',
  '🎯 Overall Goal:': '🎯 Overall Goal:',

  // ============================================================================
  // Dialogs - Auth
  // ============================================================================
  'Get started': 'Get started',
  'Use Copilot Shell': 'Use Copilot Shell',
  'How would you like to authenticate for this project?':
    'How would you like to authenticate for this project?',

  'Select authorization': 'Select authorization',
  "Choose how you'd like to authenticate in Copilot Shell.":
    "Choose how you'd like to authenticate in Copilot Shell.",

  'Continue without Copilot Shell': 'Continue without Copilot Shell',
  'Continue to Bash': 'Continue to Bash',
  'Open an interactive Bash shell without configuring AI authentication':
    'Open an interactive Bash shell without configuring AI authentication',
  'OpenAI API key is required to use OpenAI authentication.':
    'OpenAI API key is required to use OpenAI authentication.',
  'You must select an auth method to proceed. Press Ctrl+C again to exit.':
    'You must select an auth method to proceed. Press Ctrl+C again to exit.',
  'You must select an auth method or continue to Bash to proceed. Press Ctrl+C again to exit.':
    'You must select an auth method or continue to Bash to proceed. Press Ctrl+C again to exit.',
  '(Use Enter to Set Auth)': '(Use Enter to Set Auth)',
  '(↑↓ Select · Tab Switch Section · Enter Continue)':
    '(↑↓ Select · Tab Switch Section · Enter Continue)',
  '(↑↓ Select · Enter Continue)': '(↑↓ Select · Enter Continue)',
  'Terms of Services and Privacy Notice for Copilot Shell':
    'Terms of Services and Privacy Notice for Copilot Shell',
  'Copilot Shell OAuth': 'Copilot Shell OAuth',
  OpenAI: 'OpenAI',
  'Failed to login. Message: {{message}}':
    'Failed to login. Message: {{message}}',
  'Authentication is enforced to be {{enforcedType}}, but you are currently using {{currentType}}.':
    'Authentication is enforced to be {{enforcedType}}, but you are currently using {{currentType}}.',
  'Please visit this URL to authorize:': 'Please visit this URL to authorize:',
  'Or scan the QR code below:': 'Or scan the QR code below:',
  'Waiting for authorization': 'Waiting for authorization',
  'Time remaining:': 'Time remaining:',
  '(Press ESC or CTRL+C to cancel)': '(Press ESC or CTRL+C to cancel)',
  'Authentication timed out. Please try again.':
    'Authentication timed out. Please try again.',
  'Waiting for auth... (Press ESC or CTRL+C to cancel)':
    'Waiting for auth... (Press ESC or CTRL+C to cancel)',
  'Missing API key for OpenAI-compatible auth. Set settings.security.auth.apiKey, or set the {{envKeyHint}} environment variable.':
    'Missing API key for OpenAI-compatible auth. Set settings.security.auth.apiKey, or set the {{envKeyHint}} environment variable.',
  '{{envKeyHint}} environment variable not found.':
    '{{envKeyHint}} environment variable not found.',
  '{{envKeyHint}} environment variable not found. Please set it in your .env file or environment variables.':
    '{{envKeyHint}} environment variable not found. Please set it in your .env file or environment variables.',
  '{{envKeyHint}} environment variable not found (or set settings.security.auth.apiKey). Please set it in your .env file or environment variables.':
    '{{envKeyHint}} environment variable not found (or set settings.security.auth.apiKey). Please set it in your .env file or environment variables.',
  'Missing API key for OpenAI-compatible auth. Set the {{envKeyHint}} environment variable.':
    'Missing API key for OpenAI-compatible auth. Set the {{envKeyHint}} environment variable.',
  'Anthropic provider missing required baseUrl in modelProviders[].baseUrl.':
    'Anthropic provider missing required baseUrl in modelProviders[].baseUrl.',
  'ANTHROPIC_BASE_URL environment variable not found.':
    'ANTHROPIC_BASE_URL environment variable not found.',
  'Invalid auth method selected.': 'Invalid auth method selected.',
  'Failed to authenticate. Message: {{message}}':
    'Failed to authenticate. Message: {{message}}',
  'Configuration verification failed. Please check your API Key and model settings.':
    'Configuration verification failed. Please check your API Key and model settings.',
  'Verifying and saving configuration...':
    'Verifying and saving configuration...',
  'Initializing model configuration, please wait...':
    'Initializing model configuration, please wait...',
  '{{authType}} configuration saved successfully, current model: {{model}}':
    '{{authType}} configuration saved successfully, current model: {{model}}',
  '{{authType}} credentials saved successfully.':
    '{{authType}} credentials saved successfully.',
  'Kernel: {{score}}/100, {{count}} tuning suggestion(s) ({{high}} high-confidence). Explore them anytime with ktuner.':
    'Kernel: {{score}}/100, {{count}} tuning suggestion(s) ({{high}} high-confidence). Explore them anytime with ktuner.',
  'ktuner detected — read-only kernel tuning suggestions are available. Run /ktuner enable to view them, or /ktuner disable to stop asking.':
    'ktuner detected — read-only kernel tuning suggestions are available. Run /ktuner enable to view them, or /ktuner disable to stop asking.',
  'ktuner check is enabled, but no trusted ktuner binary was found on a system path. See the ktuner install docs.':
    'ktuner check is enabled, but no trusted ktuner binary was found on a system path. See the ktuner install docs.',
  'Control the read-only ktuner kernel tuning check':
    'Control the read-only ktuner kernel tuning check',
  'Enable the read-only ktuner kernel tuning check':
    'Enable the read-only ktuner kernel tuning check',
  'Stop the ktuner kernel tuning check and hint':
    'Stop the ktuner kernel tuning check and hint',
  'ktuner check enabled — running a read-only kernel scan now. Results will appear below.':
    'ktuner check enabled — running a read-only kernel scan now. Results will appear below.',
  'ktuner check enabled, but no trusted ktuner binary was found on a system path. After installing ktuner, run /ktuner enable again to view the report.':
    'ktuner check enabled, but no trusted ktuner binary was found on a system path. After installing ktuner, run /ktuner enable again to view the report.',
  'ktuner check enabled. A check has already been completed this session.':
    'ktuner check enabled. A check has already been completed this session.',
  'Kernel score: {{score}}/100. No tuning suggestions — already well configured.':
    'Kernel score: {{score}}/100. No tuning suggestions — already well configured.',
  'ktuner check disabled. Re-enable it anytime with /ktuner enable.':
    'ktuner check disabled. Re-enable it anytime with /ktuner enable.',
  'ktuner check is currently "{{mode}}". Use /ktuner enable or /ktuner disable to change it.':
    'ktuner check is currently "{{mode}}". Use /ktuner enable or /ktuner disable to change it.',
  // OpenAI API key validation errors
  'Invalid API key. Please check your API key and try again.':
    'Invalid API key. Please check your API key and try again.',
  'API key does not have permission to access this resource.':
    'API key does not have permission to access this resource.',
  'Rate limit exceeded. Please check your quota.':
    'Rate limit exceeded. Please check your quota.',
  'Model "{{model}}" is not available. Please check if the model name is correct.':
    'Model "{{model}}" is not available. Please check if the model name is correct.',
  'Invalid QWEN_DEFAULT_AUTH_TYPE value: "{{value}}". Valid values are: {{validValues}}':
    'Invalid QWEN_DEFAULT_AUTH_TYPE value: "{{value}}". Valid values are: {{validValues}}',
  'Custom Provider Configuration Required':
    'Custom Provider Configuration Required',
  'Please enter your OpenAI configuration. You can get an API key from':
    'Please enter your OpenAI configuration. You can get an API key from',
  'API Key:': 'API Key:',
  'Invalid credentials: {{errorMessage}}':
    'Invalid credentials: {{errorMessage}}',
  'Failed to validate credentials': 'Failed to validate credentials',
  'Press Enter to continue, Tab/↑↓ to navigate, Esc to cancel':
    'Press Enter to continue, Tab/↑↓ to navigate, Esc to cancel',
  'Provider:': 'Provider:',
  'Get API key from: ': 'Get API key from: ',
  'Base URL:': 'Base URL:',
  '↑↓ select provider · Enter/Tab navigate fields · Esc cancel':
    '↑↓ select provider · Enter/Tab navigate fields · Esc cancel',
  'Custom Provider': 'Custom Provider',
  'Paid · Use your own API key · Cost depends on provider':
    'Paid · Use your own API key · Cost depends on provider',
  'Custom (enter Base URL manually)': 'Custom (enter Base URL manually)',
  'Model:': 'Model:',
  'Aliyun Authentication': 'Aliyun Authentication',
  'Free with limited quota': 'Free with limited quota',
  'Please enter your Aliyun Access Key credentials. You can get them from':
    'Please enter your Aliyun Access Key credentials. You can get them from',
  'Access Key ID:': 'Access Key ID:',
  'Access Key Secret:': 'Access Key Secret:',
  'Detecting environment...': 'Detecting environment...',
  'Please click or copy the URL below to your browser to complete authentication:':
    'Please click or copy the URL below to your browser to complete authentication:',
  'ECS Instance ID:': 'ECS Instance ID:',
  'Preparing authentication...': 'Preparing authentication...',
  'Authentication failed': 'Authentication failed',
  'Press Enter to go back · Esc to cancel':
    'Press Enter to go back · Esc to cancel',
  'Timeout waiting for RAM role authorization':
    'Timeout waiting for RAM role authorization',
  'Failed to get STS credentials': 'Failed to get STS credentials',

  // ============================================================================
  // Dialogs - Model
  // ============================================================================
  'Select Model': 'Select Model',
  '(Press Esc to close)': '(Press Esc to close)',
  '(Press Esc to cancel)': '(Press Esc to cancel)',
  'Current (effective) configuration': 'Current (effective) configuration',
  AuthType: 'AuthType',
  'API Key': 'API Key',
  unset: 'unset',
  '(default)': '(default)',
  '(set)': '(set)',
  '(not set)': '(not set)',
  "Failed to switch model to '{{modelId}}'.\n\n{{error}}":
    "Failed to switch model to '{{modelId}}'.\n\n{{error}}",
  'The latest Qwen Coder model from Alibaba Cloud ModelStudio (version: qwen3-coder-plus-2025-09-23)':
    'The latest Qwen Coder model from Alibaba Cloud ModelStudio (version: qwen3-coder-plus-2025-09-23)',
  'The latest Qwen Vision model from Alibaba Cloud ModelStudio (version: qwen3-vl-plus-2025-09-23)':
    'The latest Qwen Vision model from Alibaba Cloud ModelStudio (version: qwen3-vl-plus-2025-09-23)',
  'Current /auth model': 'Current /auth model',
  'Verified via /auth': 'Verified via /auth',

  // ============================================================================
  // Dialogs - Permissions
  // ============================================================================
  'Manage folder trust settings': 'Manage folder trust settings',

  // ============================================================================
  // Status Bar
  // ============================================================================
  'Using:': 'Using:',
  '{{count}} open file': '{{count}} open file',
  '{{count}} open files': '{{count}} open files',
  '(ctrl+g to view)': '(ctrl+g to view)',
  '{{count}} {{name}} file': '{{count}} {{name}} file',
  '{{count}} {{name}} files': '{{count}} {{name}} files',
  '{{count}} MCP server': '{{count}} MCP server',
  '{{count}} MCP servers': '{{count}} MCP servers',
  '{{count}} Blocked': '{{count}} Blocked',
  '(ctrl+t to view)': '(ctrl+t to view)',
  '(ctrl+t to toggle)': '(ctrl+t to toggle)',
  'Press Ctrl+C again to exit.': 'Press Ctrl+C again to exit.',
  'Press Ctrl+D again to exit.': 'Press Ctrl+D again to exit.',
  'Press Esc again to clear.': 'Press Esc again to clear.',

  // ============================================================================
  // MCP Status
  // ============================================================================
  'No MCP servers configured.': 'No MCP servers configured.',
  'Please view MCP documentation in your browser:':
    'Please view MCP documentation in your browser:',
  'or use the cli /docs command': 'or use the cli /docs command',
  '⏳ MCP servers are starting up ({{count}} initializing)...':
    '⏳ MCP servers are starting up ({{count}} initializing)...',
  'Note: First startup may take longer. Tool availability will update automatically.':
    'Note: First startup may take longer. Tool availability will update automatically.',
  'Configured MCP servers:': 'Configured MCP servers:',
  Ready: 'Ready',
  'Starting... (first startup may take longer)':
    'Starting... (first startup may take longer)',
  Disconnected: 'Disconnected',
  '{{count}} tool': '{{count}} tool',
  '{{count}} tools': '{{count}} tools',
  '{{count}} prompt': '{{count}} prompt',
  '{{count}} prompts': '{{count}} prompts',
  '(from {{extensionName}})': '(from {{extensionName}})',
  OAuth: 'OAuth',
  'OAuth expired': 'OAuth expired',
  'OAuth not authenticated': 'OAuth not authenticated',
  'tools and prompts will appear when ready':
    'tools and prompts will appear when ready',
  '{{count}} tools cached': '{{count}} tools cached',
  'Tools:': 'Tools:',
  'Parameters:': 'Parameters:',
  'Prompts:': 'Prompts:',
  Blocked: 'Blocked',
  '💡 Tips:': '💡 Tips:',
  Use: 'Use',
  'to show server and tool descriptions':
    'to show server and tool descriptions',
  'to show tool parameter schemas': 'to show tool parameter schemas',
  'to hide descriptions': 'to hide descriptions',
  'to authenticate with OAuth-enabled servers':
    'to authenticate with OAuth-enabled servers',
  Press: 'Press',
  'to toggle tool descriptions on/off': 'to toggle tool descriptions on/off',
  "Starting OAuth authentication for MCP server '{{name}}'...":
    "Starting OAuth authentication for MCP server '{{name}}'...",
  'Restarting MCP servers...': 'Restarting MCP servers...',

  // ============================================================================
  // Startup Tips
  // ============================================================================
  'Tips:': 'Tips:',
  'Use /compress when the conversation gets long to summarize history and free up context.':
    'Use /compress when the conversation gets long to summarize history and free up context.',
  'Start a fresh idea with /clear or /new; the previous session stays available in history.':
    'Start a fresh idea with /clear or /new; the previous session stays available in history.',
  'Use /bug to submit issues to the maintainers when something goes off.':
    'Use /bug to submit issues to the maintainers when something goes off.',
  'Switch auth type quickly with /auth.':
    'Switch auth type quickly with /auth.',
  'You can run any shell commands from Copilot Shell using ! (e.g. !ls).':
    'You can run any shell commands from Copilot Shell using ! (e.g. !ls).',
  'Type / to open the command popup; Tab autocompletes slash commands and saved prompts.':
    'Type / to open the command popup; Tab autocompletes slash commands and saved prompts.',
  'You can resume a previous conversation by running co/copilot --continue or co/copilot --resume.':
    'You can resume a previous conversation by running co/copilot --continue or co/copilot --resume.',
  'You can switch permission mode quickly with Shift+Tab or /approval-mode.':
    'You can switch permission mode quickly with Shift+Tab or /approval-mode.',

  // ============================================================================
  // Exit Screen / Stats
  // ============================================================================
  'Agent powering down. Goodbye!': 'Agent powering down. Goodbye!',
  'To continue this session, run': 'To continue this session, run',
  'Interaction Summary': 'Interaction Summary',
  'Session ID:': 'Session ID:',
  'Tool Calls:': 'Tool Calls:',
  'Success Rate:': 'Success Rate:',
  'User Agreement:': 'User Agreement:',
  reviewed: 'reviewed',
  'Code Changes:': 'Code Changes:',
  'Sandbox Runs:': 'Sandbox Runs:',
  'Sandbox Blocked:': 'Sandbox Blocked:',
  Performance: 'Performance',
  'Wall Time:': 'Wall Time:',
  'Agent Active:': 'Agent Active:',
  'API Time:': 'API Time:',
  'Tool Time:': 'Tool Time:',
  'Session Stats': 'Session Stats',
  'Model Usage': 'Model Usage',
  Reqs: 'Reqs',
  'Input Tokens': 'Input Tokens',
  'Output Tokens': 'Output Tokens',
  'Savings Highlight:': 'Savings Highlight:',
  'of input tokens were served from the cache, reducing costs.':
    'of input tokens were served from the cache, reducing costs.',
  'Tip: For a full token breakdown, run `/stats model`.':
    'Tip: For a full token breakdown, run `/stats model`.',
  'Model Stats For Nerds': 'Model Stats For Nerds',
  'Tool Stats For Nerds': 'Tool Stats For Nerds',
  Metric: 'Metric',
  API: 'API',
  Requests: 'Requests',
  Errors: 'Errors',
  'Avg Latency': 'Avg Latency',
  Tokens: 'Tokens',
  Total: 'Total',
  Prompt: 'Prompt',
  Cached: 'Cached',
  Thoughts: 'Thoughts',
  Tool: 'Tool',
  Output: 'Output',
  'No API calls have been made in this session.':
    'No API calls have been made in this session.',
  'Tool Name': 'Tool Name',
  Calls: 'Calls',
  'Success Rate': 'Success Rate',
  'Avg Duration': 'Avg Duration',
  'User Decision Summary': 'User Decision Summary',
  'Total Reviewed Suggestions:': 'Total Reviewed Suggestions:',
  ' » Accepted:': ' » Accepted:',
  ' » Rejected:': ' » Rejected:',
  ' » Modified:': ' » Modified:',
  ' Overall Agreement Rate:': ' Overall Agreement Rate:',
  'No tool calls have been made in this session.':
    'No tool calls have been made in this session.',
  'Session start time is unavailable, cannot calculate stats.':
    'Session start time is unavailable, cannot calculate stats.',

  // ============================================================================
  // Command Format Migration
  // ============================================================================
  'Command Format Migration': 'Command Format Migration',
  'Found {{count}} TOML command file:': 'Found {{count}} TOML command file:',
  'Found {{count}} TOML command files:': 'Found {{count}} TOML command files:',
  '... and {{count}} more': '... and {{count}} more',
  'The TOML format is deprecated. Would you like to migrate them to Markdown format?':
    'The TOML format is deprecated. Would you like to migrate them to Markdown format?',
  '(Backups will be created and original files will be preserved)':
    '(Backups will be created and original files will be preserved)',

  // ============================================================================
  // Loading Phrases
  // ============================================================================
  'Waiting for user confirmation...': 'Waiting for user confirmation...',
  '(esc to cancel, {{time}})': '(esc to cancel, {{time}})',

  // ============================================================================
  // Loading Phrases
  // ============================================================================
  WITTY_LOADING_PHRASES: [
    "I'm Feeling Lucky",
    'Shipping awesomeness... ',
    'Painting the serifs back on...',
    'Navigating the slime mold...',
    'Consulting the digital spirits...',
    'Reticulating splines...',
    'Warming up the AI hamsters...',
    'Asking the magic conch shell...',
    'Generating witty retort...',
    'Polishing the algorithms...',
    "Don't rush perfection (or my code)...",
    'Brewing fresh bytes...',
    'Counting electrons...',
    'Engaging cognitive processors...',
    'Checking for syntax errors in the universe...',
    'One moment, optimizing humor...',
    'Shuffling punchlines...',
    'Untangling neural nets...',
    'Compiling brilliance...',
    'Loading wit.exe...',
    'Summoning the cloud of wisdom...',
    'Preparing a witty response...',
    "Just a sec, I'm debugging reality...",
    'Confuzzling the options...',
    'Tuning the cosmic frequencies...',
    'Crafting a response worthy of your patience...',
    'Compiling the 1s and 0s...',
    'Resolving dependencies... and existential crises...',
    'Defragmenting memories... both RAM and personal...',
    'Rebooting the humor module...',
    'Caching the essentials (mostly cat memes)...',
    'Optimizing for ludicrous speed',
    "Swapping bits... don't tell the bytes...",
    'Garbage collecting... be right back...',
    'Assembling the interwebs...',
    'Converting coffee into code...',
    'Updating the syntax for reality...',
    'Rewiring the synapses...',
    'Looking for a misplaced semicolon...',
    "Greasin' the cogs of the machine...",
    'Pre-heating the servers...',
    'Calibrating the flux capacitor...',
    'Engaging the improbability drive...',
    'Channeling the Force...',
    'Aligning the stars for optimal response...',
    'So say we all...',
    'Loading the next great idea...',
    "Just a moment, I'm in the zone...",
    'Preparing to dazzle you with brilliance...',
    "Just a tick, I'm polishing my wit...",
    "Hold tight, I'm crafting a masterpiece...",
    "Just a jiffy, I'm debugging the universe...",
    "Just a moment, I'm aligning the pixels...",
    "Just a sec, I'm optimizing the humor...",
    "Just a moment, I'm tuning the algorithms...",
    'Warp speed engaged...',
    'Mining for more Dilithium crystals...',
    "Don't panic...",
    'Following the white rabbit...',
    'The truth is in here... somewhere...',
    'Blowing on the cartridge...',
    'Loading... Do a barrel roll!',
    'Waiting for the respawn...',
    'Finishing the Kessel Run in less than 12 parsecs...',
    "The cake is not a lie, it's just still loading...",
    'Fiddling with the character creation screen...',
    "Just a moment, I'm finding the right meme...",
    "Pressing 'A' to continue...",
    'Herding digital cats...',
    'Polishing the pixels...',
    'Finding a suitable loading screen pun...',
    'Distracting you with this witty phrase...',
    'Almost there... probably...',
    'Our hamsters are working as fast as they can...',
    'Giving Cloudy a pat on the head...',
    'Petting the cat...',
    'Rickrolling my boss...',
    'Never gonna give you up, never gonna let you down...',
    'Slapping the bass...',
    'Tasting the snozberries...',
    "I'm going the distance, I'm going for speed...",
    'Is this the real life? Is this just fantasy?...',
    "I've got a good feeling about this...",
    'Poking the bear...',
    'Doing research on the latest memes...',
    'Figuring out how to make this more witty...',
    'Hmmm... let me think...',
    'What do you call a fish with no eyes? A fsh...',
    'Why did the computer go to therapy? It had too many bytes...',
    "Why don't programmers like nature? It has too many bugs...",
    'Why do programmers prefer dark mode? Because light attracts bugs...',
    'Why did the developer go broke? Because they used up all their cache...',
    "What can you do with a broken pencil? Nothing, it's pointless...",
    'Applying percussive maintenance...',
    'Searching for the correct USB orientation...',
    'Ensuring the magic smoke stays inside the wires...',
    'Trying to exit Vim...',
    'Spinning up the hamster wheel...',
    "That's not a bug, it's an undocumented feature...",
    'Engage.',
    "I'll be back... with an answer.",
    'My other process is a TARDIS...',
    'Communing with the machine spirit...',
    'Letting the thoughts marinate...',
    'Just remembered where I put my keys...',
    'Pondering the orb...',
    "I've seen things you people wouldn't believe... like a user who reads loading messages.",
    'Initiating thoughtful gaze...',
    "What's a computer's favorite snack? Microchips.",
    "Why do Java developers wear glasses? Because they don't C#.",
    'Charging the laser... pew pew!',
    'Dividing by zero... just kidding!',
    'Looking for an adult superviso... I mean, processing.',
    'Making it go beep boop.',
    'Buffering... because even AIs need a moment.',
    'Entangling quantum particles for a faster response...',
    'Polishing the chrome... on the algorithms.',
    'Are you not entertained? (Working on it!)',
    'Summoning the code gremlins... to help, of course.',
    'Just waiting for the dial-up tone to finish...',
    'Recalibrating the humor-o-meter.',
    'My other loading screen is even funnier.',
    "Pretty sure there's a cat walking on the keyboard somewhere...",
    'Enhancing... Enhancing... Still loading.',
    "It's not a bug, it's a feature... of this loading screen.",
    'Have you tried turning it off and on again? (The loading screen, not me.)',
    'Constructing additional pylons...',
  ],

  // ============================================================================
  // Extension Settings Input
  // ============================================================================
  'Enter value...': 'Enter value...',
  'Enter sensitive value...': 'Enter sensitive value...',
  'Press Enter to submit, Escape to cancel':
    'Press Enter to submit, Escape to cancel',

  // ============================================================================
  // Command Migration Tool
  // ============================================================================
  'Markdown file already exists: {{filename}}':
    'Markdown file already exists: {{filename}}',
  'TOML Command Format Deprecation Notice':
    'TOML Command Format Deprecation Notice',
  'Found {{count}} command file(s) in TOML format:':
    'Found {{count}} command file(s) in TOML format:',
  'The TOML format for commands is being deprecated in favor of Markdown format.':
    'The TOML format for commands is being deprecated in favor of Markdown format.',
  'Markdown format is more readable and easier to edit.':
    'Markdown format is more readable and easier to edit.',
  'You can migrate these files automatically using:':
    'You can migrate these files automatically using:',
  'Or manually convert each file:': 'Or manually convert each file:',
  'TOML: prompt = "..." / description = "..."':
    'TOML: prompt = "..." / description = "..."',
  'Markdown: YAML frontmatter + content':
    'Markdown: YAML frontmatter + content',
  'The migration tool will:': 'The migration tool will:',
  'Convert TOML files to Markdown': 'Convert TOML files to Markdown',
  'Create backups of original files': 'Create backups of original files',
  'Preserve all command functionality': 'Preserve all command functionality',
  'TOML format will continue to work for now, but migration is recommended.':
    'TOML format will continue to work for now, but migration is recommended.',

  // ============================================================================
  // Extensions - Explore Command
  // ============================================================================
  'Open extensions page in your browser':
    'Open extensions page in your browser',
  'Unknown extensions source: {{source}}.':
    'Unknown extensions source: {{source}}.',
  'Would open extensions page in your browser: {{url}} (skipped in test environment)':
    'Would open extensions page in your browser: {{url}} (skipped in test environment)',
  'View available extensions at {{url}}':
    'View available extensions at {{url}}',
  'Opening extensions page in your browser: {{url}}':
    'Opening extensions page in your browser: {{url}}',
  'Failed to open browser. Check out the extensions gallery at {{url}}':
    'Failed to open browser. Check out the extensions gallery at {{url}}',

  // ============================================================================
  // Components - Show More Lines
  // ============================================================================
  'Press Ctrl-S to show more lines': 'Press Ctrl-S to show more lines',

  // ============================================================================
  // Commands - Bash
  // ============================================================================
  'Launch an interactive bash shell; exit to return to the TUI':
    'Launch an interactive bash shell; exit to return',
  '/bash is only available in interactive mode.':
    '/bash is only available in interactive mode.',
  '/bash only accepts a shell executable (e.g. /bash, /bash zsh, /bash fish). To run a command, just type it directly.':
    '/bash only accepts a shell executable (e.g. /bash, /bash zsh, /bash fish). To run a command, just type it directly.',
  '"{{shell}}" does not appear to be a valid interactive shell. Use a shell name like bash, zsh, or fish.':
    '"{{shell}}" does not appear to be a valid interactive shell. Use a shell name like bash, zsh, or fish.',
  "Entering interactive shell. Type 'exit' or press Ctrl+D to return.":
    "Entering interactive shell. Type 'exit' or press Ctrl+D to return.",
  'Failed to spawn shell: {{error}}': 'Failed to spawn shell: {{error}}',
  'Returned from shell. Restoring TUI...':
    'Returned from shell. Restoring session...',
  'Interactive shell is not supported in non-interactive mode.':
    'Interactive shell is not supported in non-interactive mode.',
  'Nested session detected: you are attempting to start a new session. The new session will not contain the original context.':
    'Nested session detected: you are attempting to start a new session. The new session will not contain the original context.',
  'Press Ctrl+C to exit, then type exit to return to the original session.':
    'Press Ctrl+C to exit, then type exit to return to the original session.',
  'Press Enter to start a new session.': 'Press Enter to start a new session.',
  'Use /bash to switch to an interactive Bash shell at any time. Type "exit" or press Ctrl+D to return to Copilot Shell.':
    'Use /bash to switch to an interactive Bash shell at any time. Type "exit" or press Ctrl+D to return to Copilot Shell.',
  'Use /dir cd <path> to switch the current working directory without leaving Copilot Shell.':
    'Use /dir cd <path> to switch the current working directory without leaving Copilot Shell.',
  'Use /cosh-switch or run "cosh-switch" in bash to switch between copilot-shell and cosh-ng.':
    'Use /cosh-switch or run "cosh-switch" in bash to switch between copilot-shell and cosh-ng.',
  // Agent Key sharing flow
  '{{agentName}} configuration detected':
    '{{agentName}} configuration detected',
  'The following configuration from {{agentName}} will be imported':
    'The following configuration from {{agentName}} will be imported',
  'Press Enter or wait 2s to complete authentication':
    'Press Enter or wait 2s to complete authentication',
  'Agent Key Sharing': 'Agent Key Sharing',
  'Authorize importing API keys from the configuration file of installed agents for automatic configuration? Choose an Agent or manually configure it.':
    'Authorize importing API keys from the configuration file of installed agents for automatic configuration? Choose an Agent or manually configure it.',
  'No, configure manually': 'No, configure manually',
  'No API Key found for {{agentName}}.': 'No API Key found for {{agentName}}.',
  'Returning to agent selection...': 'Returning to agent selection...',
  'Press Enter or wait 2s to continue': 'Press Enter or wait 2s to continue',
  '↑↓ or j/k to navigate · 1/2/3 select · Enter confirm · Esc cancel':
    '↑↓ or j/k to navigate · 1/2/3 select · Enter confirm · Esc cancel',
  "Set up Copilot Shell's status line UI":
    "Set up Copilot Shell's status line UI",
  'Show current status line configuration':
    'Show current status line configuration',
  'Clear status line configuration': 'Clear status line configuration',
  'Current status line command: {{command}}':
    'Current status line command: {{command}}',
  'No status line command is currently set.':
    'No status line command is currently set.',
  'Status line command cleared.': 'Status line command cleared.',
  'Status line command set to: {{command}}':
    'Status line command set to: {{command}}',
  'Toggle compact mode': 'Toggle compact mode',
  'Hide tool output and thinking for a cleaner view (toggle with Ctrl+O)':
    'Hide tool output and thinking for a cleaner view (toggle with Ctrl+O)',
  verbose: 'verbose',
  // Export command
  'Export current session message history to a file':
    'Export current session message history to a file',
  'Export to Markdown format': 'Export to Markdown format',
  'Export to HTML format': 'Export to HTML format',
  'Export to JSON format': 'Export to JSON format',
  'Export to JSONL format': 'Export to JSONL format',
  'Could not determine current working directory.':
    'Could not determine current working directory.',
  'No active session found to export.': 'No active session found to export.',
  'Session exported to markdown: {{filename}}':
    'Session exported to markdown: {{filename}}',
  'Session exported to HTML: {{filename}}':
    'Session exported to HTML: {{filename}}',
  'Session exported to JSON: {{filename}}':
    'Session exported to JSON: {{filename}}',
  'Session exported to JSONL: {{filename}}':
    'Session exported to JSONL: {{filename}}',
  'Failed to export session: {{error}}': 'Failed to export session: {{error}}',
  // ESC cancel
  'Command cancelled.': 'Command cancelled.',
  // Clawhub
  'Manage skills via clawhub CLI': 'Manage skills via clawhub CLI',
  'Search skills in the registry': 'Search skills in the registry',
  'Install a skill': 'Install a skill',
  'Uninstall a skill': 'Uninstall a skill',
  'Update skill(s). Usage: update <slug> | update --all':
    'Update skill(s). Usage: update <slug> | update --all',
  'List installed skills': 'List installed skills',
  'View skill details': 'View skill details',
  'Login to clawhub': 'Login to clawhub',
  'Usage: /clawhub login <token>': 'Usage: /clawhub login <token>',
  'Show current identity': 'Show current identity',
  'clawhub CLI is not installed. Install it now via npm install -g clawhub?':
    'clawhub CLI is not installed. Install it now via npm install -g clawhub?',
  'Installing clawhub via npm install -g clawhub …':
    'Installing clawhub via npm install -g clawhub …',
  'clawhub installed successfully.': 'clawhub installed successfully.',
  'Failed to install clawhub: {{error}}\nPlease install manually: npm install -g clawhub':
    'Failed to install clawhub: {{error}}\nPlease install manually: npm install -g clawhub',
  'clawhub exited with code {{code}}.': 'clawhub exited with code {{code}}.',
  'Hint: Run `clawhub login` to authenticate and bypass rate limits.':
    'Hint: Run `clawhub login` to authenticate and bypass rate limits.',
  'No results.': 'No results.',
  'Command completed successfully.': 'Command completed successfully.',
  'Clawhub Search': 'Clawhub Search',
  'Clawhub Install': 'Clawhub Install',
  'Clawhub Uninstall': 'Clawhub Uninstall',
  'Clawhub Update': 'Clawhub Update',
  'Clawhub Installed Skills': 'Clawhub Installed Skills',
  'Clawhub Inspect': 'Clawhub Inspect',
  'Clawhub Login': 'Clawhub Login',
  'Clawhub Identity': 'Clawhub Identity',
  'Usage: /clawhub <subcommand> [args]': 'Usage: /clawhub <subcommand> [args]',
  'Subcommands:': 'Subcommands:',
  'Search skills in the registry (subcommand help)':
    '  search <keyword>        Search skills in the registry',
  'Install a skill (subcommand help)':
    '  install <slug>          Install a skill',
  'Uninstall a skill (subcommand help)':
    '  uninstall <slug>        Uninstall a skill',
  'Update skills (subcommand help)':
    '  update <slug|--all>     Update skill(s)',
  'List installed skills (subcommand help)':
    '  list                    List installed skills',
  'View details (subcommand help)':
    '  inspect <slug>          View skill details',
  'Login (subcommand help)': '  login <token>           Login to clawhub',
  'Show identity (subcommand help)':
    '  whoami                  Show current identity',

  // ============================================================================
  // Hook dialogs
  // ============================================================================
  'Hook "{{name}}" has been {{action}} for this session.':
    'Hook "{{name}}" has been {{action}} for this session.',
  '{{verb}} {{count}} hooks for this session: {{names}}':
    '{{verb}} {{count}} hooks for this session: {{names}}',
  'No matching hooks found: {{names}}': 'No matching hooks found: {{names}}',
  'Unknown hooks: {{names}}': 'Unknown hooks: {{names}}',
  'No disabled hooks to enable.': 'No disabled hooks to enable.',
  'No enabled hooks to disable.': 'No enabled hooks to disable.',
  'Select hooks to enable': 'Select hooks to enable',
  'Select hooks to disable': 'Select hooks to disable',
  'Enable disabled hooks': 'Enable disabled hooks',
  'Disable active hooks': 'Disable active hooks',
  '{{count}} selected': '{{count}} selected',
  '↑↓/jk navigate, Space toggle, a select all, Enter confirm, Esc cancel':
    '↑↓/jk navigate, Space toggle, a select all, Enter confirm, Esc cancel',
  Enabled: 'Enabled',
  Disabled: 'Disabled',
  enabled: 'enabled',
  disabled: 'disabled',
  '⚠️  **Hook Safety Check**': '⚠️  **Hook Safety Check**',
  'Send this prompt or cancel?': 'Send this prompt or cancel?',
  'A hook requires your confirmation to proceed.':
    'A hook requires your confirmation to proceed.',
};
