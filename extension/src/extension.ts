import * as vscode from 'vscode';
import { spawn, ChildProcess, execFile } from 'child_process';
import * as path from 'path';

/**
 * LCode VS Code extension.
 *
 * Commands:
 *  - lcode.runTask:          prompt for a task description, run `lcode run <task> -y`,
 *                            stream stdout/stderr into a terminal-style webview panel.
 *  - lcode.explainSelection: turn the current editor selection into an "explain" task
 *                            and show the result in the same panel.
 *
 * Configuration:
 *  - lcode.path:      path to the lcode binary (default "lcode", i.e. on PATH).
 *  - lcode.maxTurns:  max agent turns per task (default 50, 0 = unlimited).
 */

const SPINNER_FRAMES = ['◐', '◓', '◑', '◒'];
const ANSI_ESCAPE = /[\u001b\u009b][[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]/g;

interface LcodeSession {
    readonly id: number;
    readonly task: string;
    readonly startedAt: number;
    child?: ChildProcess;
    finished: boolean;
}

let currentPanel: vscode.WebviewPanel | undefined;
let currentSession: LcodeSession | undefined;
let sessionCounter = 0;

let statusBar: vscode.StatusBarItem;
let spinnerTimer: NodeJS.Timeout | undefined;
let spinnerFrame = 0;

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

export function activate(context: vscode.ExtensionContext): void {
    statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    statusBar.command = 'lcode.runTask';
    context.subscriptions.push(statusBar);

    context.subscriptions.push(
        vscode.commands.registerCommand('lcode.runTask', runTask),
        vscode.commands.registerCommand('lcode.explainSelection', explainSelection),
    );
}

export function deactivate(): void {
    currentSession?.child?.kill();
    stopSpinner();
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async function runTask(): Promise<void> {
    const task = await vscode.window.showInputBox({
        title: 'LCode',
        prompt: 'Describe the task for LCode',
        placeHolder: 'e.g. Add unit tests for the auth module',
        ignoreFocusOut: true,
        validateInput: (value: string) =>
            value.trim().length === 0 ? 'Task description cannot be empty.' : undefined,
    });
    if (task === undefined) {
        return; // user cancelled
    }
    await startSession(task.trim());
}

async function explainSelection(): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
        vscode.window.showWarningMessage('LCode: no active editor. Open a file and select some code first.');
        return;
    }
    if (editor.selection.isEmpty) {
        vscode.window.showWarningMessage('LCode: nothing selected. Select the code you want explained first.');
        return;
    }

    const fileName = path.basename(editor.document.fileName);
    let snippet = editor.document.getText(editor.selection);
    const MAX_SNIPPET = 8000;
    if (snippet.length > MAX_SNIPPET) {
        snippet = `${snippet.slice(0, MAX_SNIPPET)}\n// ... (selection truncated to ${MAX_SNIPPET} chars)`;
    }

    const task = `Explain the following code (file: ${fileName}):\n\n${snippet}`;
    await startSession(task);
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

async function startSession(task: string): Promise<void> {
    const bin = getLcodePath();

    if (!(await binaryExists(bin))) {
        await promptBinaryMissing(bin);
        return;
    }

    // A previous task may still be running: terminate it and start fresh.
    if (currentSession && !currentSession.finished) {
        currentSession.child?.kill();
    }

    const session: LcodeSession = {
        id: ++sessionCounter,
        task,
        startedAt: Date.now(),
        finished: false,
    };
    currentSession = session;

    const panel = ensurePanel();
    postToPanel(panel, 'sessionStart', {
        id: session.id,
        task,
        bin,
        startedAt: session.startedAt,
    });

    startSpinner();

    const maxTurns = vscode.workspace.getConfiguration('lcode').get<number>('maxTurns', 50);
    const args = ['run', task, '-y'];
    if (maxTurns > 0) {
        args.push('-n', String(maxTurns));
    }

    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    const child = spawn(bin, args, { cwd, env: process.env });
    session.child = child;

    let hadStderr = false;

    child.stdout.on('data', (chunk: Buffer) => {
        postToPanel(panel, 'output', { kind: 'stdout', text: stripAnsi(chunk.toString()) });
    });
    child.stderr.on('data', (chunk: Buffer) => {
        hadStderr = true;
        postToPanel(panel, 'output', { kind: 'stderr', text: stripAnsi(chunk.toString()) });
    });

    child.on('error', (err: NodeJS.ErrnoException) => {
        if (session !== currentSession) {
            return; // superseded by a newer session
        }
        session.finished = true;
        stopSpinner();
        const elapsedMs = Date.now() - session.startedAt;
        if (err.code === 'ENOENT') {
            postToPanel(panel, 'sessionEnd', {
                code: -2,
                error: `lcode binary not found: "${bin}"`,
                elapsedMs,
            });
            void promptBinaryMissing(bin);
        } else {
            postToPanel(panel, 'sessionEnd', { code: -1, error: err.message, elapsedMs });
            void vscode.window.showErrorMessage(`LCode failed to start: ${err.message}`);
        }
    });

    child.on('close', (code: number | null) => {
        if (session !== currentSession) {
            return; // superseded by a newer session
        }
        session.finished = true;
        stopSpinner();
        postToPanel(panel, 'sessionEnd', {
            code: code ?? -1,
            error: hadStderr ? 'Process finished with output on stderr (see orange lines).' : undefined,
            elapsedMs: Date.now() - session.startedAt,
        });
        if (code !== 0) {
            void vscode.window.showWarningMessage(
                `LCode task finished with exit code ${code}. Details in the LCode output panel.`,
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Binary / config helpers
// ---------------------------------------------------------------------------

function getLcodePath(): string {
    const configured = vscode.workspace.getConfiguration('lcode').get<string>('path', 'lcode');
    return configured.trim().length > 0 ? configured.trim() : 'lcode';
}

function binaryExists(bin: string): Promise<boolean> {
    return new Promise((resolve) => {
        execFile(bin, ['--version'], { timeout: 10_000 }, (err) => {
            resolve(err === null || (err as NodeJS.ErrnoException).code !== 'ENOENT');
        });
    });
}

async function promptBinaryMissing(bin: string): Promise<void> {
    const choice = await vscode.window.showErrorMessage(
        `LCode binary not found: "${bin}". Install the LCode CLI or set "lcode.path" in settings.`,
        'Open install guide',
    );
    if (choice === 'Open install guide') {
        const doc = await vscode.workspace.openTextDocument({
            language: 'markdown',
            content: [
                '# Installing the LCode CLI',
                '',
                'This extension drives the `lcode` command-line agent. The binary is not bundled, so you must install it first.',
                '',
                '## Option 1: Build from source (cargo)',
                '',
                '```bash',
                'git clone git@github.com:Lixiang9716/LCode.git',
                'cd LCode',
                'cargo install --path .',
                '```',
                '',
                '## Option 2: Configure the extension',
                '',
                'If the binary lives somewhere else, set the VS Code setting `lcode.path` to its absolute path:',
                '',
                '```json',
                '{ "lcode.path": "/absolute/path/to/lcode" }',
                '```',
                '',
                'Then run the `LCode: Run Task` command again.',
            ].join('\n'),
        });
        void vscode.window.showTextDocument(doc);
    }
}

function stripAnsi(text: string): string {
    return text.replace(ANSI_ESCAPE, '');
}

// ---------------------------------------------------------------------------
// Status bar spinner
// ---------------------------------------------------------------------------

function startSpinner(): void {
    stopSpinner();
    spinnerFrame = 0;
    updateSpinner();
    spinnerTimer = setInterval(updateSpinner, 200);
    statusBar.show();
}

function stopSpinner(): void {
    if (spinnerTimer !== undefined) {
        clearInterval(spinnerTimer);
        spinnerTimer = undefined;
    }
    statusBar.hide();
}

function updateSpinner(): void {
    const task = currentSession?.task ?? 'task';
    const frame = SPINNER_FRAMES[spinnerFrame % SPINNER_FRAMES.length];
    spinnerFrame++;
    statusBar.text = `${frame} lcode: ${task.length > 40 ? `${task.slice(0, 40)}…` : task}`;
    statusBar.tooltip = `LCode is running: ${currentSession?.task ?? ''}`;
}

// ---------------------------------------------------------------------------
// Webview panel
// ---------------------------------------------------------------------------

interface OutboundMessage {
    type: string;
    [key: string]: unknown;
}

let pendingMessages: OutboundMessage[] = [];
let webviewReady = false;

function ensurePanel(): vscode.WebviewPanel {
    if (currentPanel) {
        currentPanel.reveal(vscode.ViewColumn.Beside);
        return currentPanel;
    }

    const panel = vscode.window.createWebviewPanel(
        'lcodeOutput',
        'LCode Agent',
        vscode.ViewColumn.Beside,
        { enableScripts: true, retainContextWhenHidden: true },
    );

    panel.webview.html = getPanelHtml(panel.webview);

    webviewReady = false;
    pendingMessages = [];
    panel.webview.onDidReceiveMessage((msg: { type: string }) => {
        if (msg.type === 'ready') {
            webviewReady = true;
            for (const m of pendingMessages) {
                void panel.webview.postMessage(m);
            }
            pendingMessages = [];
        }
    });

    panel.onDidDispose(() => {
        currentSession?.child?.kill();
        currentSession = undefined;
        currentPanel = undefined;
        stopSpinner();
    });

    currentPanel = panel;
    return panel;
}

function postToPanel(panel: vscode.WebviewPanel, type: string, payload: Record<string, unknown>): void {
    const msg: OutboundMessage = { type, ...payload };
    if (webviewReady) {
        void panel.webview.postMessage(msg);
    } else {
        pendingMessages.push(msg);
    }
}

function getPanelHtml(webview: vscode.Webview): string {
    const nonce = getNonce();
    const cspSource = webview.cspSource;
    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${cspSource}; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>LCode Agent</title>
<style>
    :root { color-scheme: dark; }
    * { box-sizing: border-box; }
    body {
        margin: 0;
        padding: 10px 14px 20px;
        background: #0d1117;
        color: #c9d1d9;
        font-family: 'Cascadia Mono', 'JetBrains Mono', Consolas, 'Courier New', monospace;
        font-size: 13px;
        line-height: 1.5;
    }
    #toolbar {
        position: sticky; top: 0; z-index: 10;
        display: flex; align-items: center; gap: 10px;
        background: #0d1117;
        padding: 6px 0 10px;
        border-bottom: 1px solid #21262d;
        margin-bottom: 10px;
    }
    button {
        background: #21262d; color: #c9d1d9;
        border: 1px solid #30363d; border-radius: 4px;
        padding: 3px 10px; font-size: 12px; cursor: pointer;
        font-family: inherit;
    }
    button:hover { background: #30363d; }
    #badge {
        font-size: 11px; font-weight: 600; letter-spacing: 1px;
        padding: 3px 10px; border-radius: 10px; white-space: nowrap;
    }
    #badge.idle { background: #21262d; color: #8b949e; }
    #badge.running { background: #1f3a5f; color: #58a6ff; animation: pulse 1.2s ease-in-out infinite; }
    #badge.ok { background: #0f3d1f; color: #3fb950; }
    #badge.failed { background: #3d1010; color: #f85149; }
    @keyframes pulse { 50% { opacity: 0.55; } }
    #task {
        flex: 1; min-width: 0;
        color: #e6edf3; white-space: pre-wrap; word-break: break-word;
        font-size: 13px; font-weight: 600;
    }
    #meta { color: #6e7681; font-size: 11px; margin: 4px 0 10px; }
    #output { overflow-y: auto; }
    .line { display: flex; gap: 10px; white-space: pre-wrap; word-break: break-word; padding: 1px 0; }
    .ts { color: #6e7681; flex: 0 0 auto; user-select: none; }
    .stdout { color: #c9d1d9; }
    .stderr { color: #f0883e; }
    .error { color: #f85149; }
    #empty { color: #6e7681; padding: 20px 0; }
</style>
</head>
<body>
    <div id="toolbar">
        <span id="badge" class="idle">IDLE</span>
        <span id="task">No task yet — run "LCode: Run Task" or "LCode: Explain Selection".</span>
        <button id="copy" title="Copy all output to clipboard">Copy</button>
        <button id="clear" title="Clear the output">Clear</button>
    </div>
    <div id="meta"></div>
    <div id="output"><div id="empty">Waiting for a task…</div></div>
    <script nonce="${nonce}">
        (function () {
            const vscode = acquireVsCodeApi();
            const outputEl = document.getElementById('output');
            const badgeEl = document.getElementById('badge');
            const taskEl = document.getElementById('task');
            const metaEl = document.getElementById('meta');
            const emptyEl = document.getElementById('empty');
            let pending = '';
            let allText = '';
            let stick = true;

            outputEl.addEventListener('scroll', function () {
                stick = outputEl.scrollHeight - outputEl.scrollTop - outputEl.clientHeight < 40;
            });

            function setBadge(state, text) {
                badgeEl.className = state;
                badgeEl.textContent = text;
            }

            function addLine(text, kind) {
                if (emptyEl) emptyEl.remove();
                const line = document.createElement('div');
                line.className = 'line';
                const ts = document.createElement('span');
                ts.className = 'ts';
                ts.textContent = new Date().toLocaleTimeString();
                const body = document.createElement('span');
                body.className = kind === 'stderr' ? 'stderr' : (kind === 'error' ? 'error' : 'stdout');
                body.textContent = text;
                line.appendChild(ts);
                line.appendChild(body);
                outputEl.appendChild(line);
                allText += '[' + ts.textContent + '] ' + text + '\\n';
                if (stick) outputEl.scrollTop = outputEl.scrollHeight;
            }

            function appendChunk(text, kind) {
                pending += text;
                let idx;
                while ((idx = pending.indexOf('\\n')) !== -1) {
                    const line = pending.slice(0, idx);
                    pending = pending.slice(idx + 1);
                    if (line) addLine(line, kind);
                }
                if (stick) outputEl.scrollTop = outputEl.scrollHeight;
            }

            function flushPending() {
                if (pending) { addLine(pending, 'stdout'); pending = ''; }
            }

            window.addEventListener('message', function (event) {
                const msg = event.data;
                if (msg.type === 'sessionStart') {
                    outputEl.textContent = '';
                    allText = '';
                    pending = '';
                    taskEl.textContent = msg.task;
                    const started = new Date(msg.startedAt).toLocaleTimeString();
                    metaEl.textContent = 'task #' + msg.id + ' · ' + started + ' · bin: ' + msg.bin;
                    setBadge('running', '● RUNNING');
                } else if (msg.type === 'output') {
                    appendChunk(msg.text, msg.kind);
                } else if (msg.type === 'sessionEnd') {
                    flushPending();
                    const elapsed = (msg.elapsedMs / 1000).toFixed(1) + 's';
                    if (msg.error) {
                        addLine(msg.error, 'error');
                        setBadge('failed', '● FAILED · exit ' + msg.code + ' · ' + elapsed);
                    } else if (msg.code === 0) {
                        setBadge('ok', '● COMPLETED · ' + elapsed);
                    } else {
                        setBadge('failed', '● FAILED · exit ' + msg.code + ' · ' + elapsed);
                    }
                    metaEl.textContent += ' · finished ' + new Date().toLocaleTimeString();
                    taskEl.textContent = msg.task;
                }
            });

            document.getElementById('copy').addEventListener('click', function () {
                navigator.clipboard.writeText(allText).catch(function () { });
            });
            document.getElementById('clear').addEventListener('click', function () {
                outputEl.textContent = '';
                allText = '';
                pending = '';
            });

            vscode.postMessage({ type: 'ready' });
        })();
    </script>
</body>
</html>`;
}

function getNonce(): string {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
    let result = '';
    for (let i = 0; i < 32; i++) {
        result += chars.charAt(Math.floor(Math.random() * chars.length));
    }
    return result;
}
