# LCode for VS Code

Run [LCode](https://github.com/Lixiang9716/LCode) AI coding tasks directly from the editor. Select code or describe a task, and watch the agent's output stream into a terminal-style panel.

## Features

- **`LCode: Run Task`** — prompts for a task description, then runs `lcode run "<task>" -y` and streams the agent's stdout/stderr live into a webview panel.
- **`LCode: Explain Selection`** — sends the current editor selection to LCode as an "Explain the following code…" task, shown in the same panel.
- Terminal-style output panel with per-line timestamps, auto-scroll, copy and clear buttons, and a completion badge (COMPLETED / FAILED with exit code and elapsed time).
- Status bar spinner while a task is running.
- Friendly error message with an install guide if the `lcode` binary is not found.

## Requirements

- VS Code >= 1.90
- The `lcode` CLI binary (the extension shells out to it; it is **not** bundled)

### Installing the LCode CLI

```bash
git clone git@github.com:Lixiang9716/LCode.git
cd LCode
cargo install --path .
```

## Installation

### Debug (F5)

1. Open this `extension/` folder in VS Code.
2. Run `npm install`.
3. Press **F5** to launch the Extension Development Host.
4. In the new window, run `LCode: Run Task` from the Command Palette.

### Package with vsce

```bash
cd extension
npm install
npm install -g @vscode/vsce   # or: npx @vscode/vsce
vsce package
code --install-extension lcode-vscode-0.1.0.vsix
```

## Configuration

| Setting        | Default  | Description                                              |
| -------------- | -------- | -------------------------------------------------------- |
| `lcode.path`   | `lcode`  | Path to the lcode binary. Use an absolute path if it is not on PATH. |
| `lcode.maxTurns` | `50`    | Max agent turns per task (`-n`). `0` means unlimited.    |

Example `settings.json`:

```json
{
  "lcode.path": "/home/me/.cargo/bin/lcode",
  "lcode.maxTurns": 80
}
```

## Usage

1. Make sure the LCode CLI is installed and on PATH (see above).
2. Open the Command Palette (`Ctrl+Shift+P`).
3. Run **`LCode: Run Task`**, type a description (e.g. *"Add unit tests for the auth module"*), and press Enter.
4. The output panel opens beside the editor and streams the agent's progress live. The task runs with `-y` (auto-approve), like the CLI's non-interactive mode.
5. To explain code: select a range in the editor, then run **`LCode: Explain Selection`**.

## Notes

- Tasks run with auto-approval (`-y`) and **may modify files** in your workspace. Use with care.
- The agent runs in the workspace root folder (the first workspace folder, if any).
- If the binary is missing, the extension shows an install guide and points to the `lcode.path` setting.
- The output panel is terminal-style: timestamps per line, orange lines are stderr, red lines are errors.
