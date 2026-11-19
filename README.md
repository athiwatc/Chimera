# Chimera

Chimera is a local MCP server written in Rust. It gives Codex, Claude Code, OpenCode, and other stdio MCP clients a common way to find saved chats across harnesses, read transcripts, and send tasks to fresh agent sessions.

## Set up

Install Rust 1.88 or later and run `cargo build --release --locked` in the Chimera checkout. The resulting executable is `target/release/chimera`. It runs without Node.js.

Set `/absolute/path/to/project` to the directory whose sessions you want to share. Install the harnesses you want Chimera to connect to.

| Harness | Connection and requirements |
| --- | --- |
| Codex | Uses Codex App Server for history and delegation. The CLI must support `thread/items/list`. |
| Claude Code | Reads history from local transcripts under `~/.claude/projects` or `$CLAUDE_CONFIG_DIR/projects`. Delegates over the CLI's streaming JSON protocol and requires `--restricted` support. |
| OpenCode | Starts `opencode serve` on authenticated loopback and uses the local session API for history and delegation. Requires OpenCode 2.x. |

Register from the project you want to share:

```sh
/absolute/path/to/Chimera/target/release/chimera register
```

Choose Codex, Claude Code, or OpenCode in the picker. Chimera uses its own absolute
executable path and the current project directory to complete registration. Use
`q`, EOF, or Ctrl+C to cancel the picker before any changes are made.

For scripts, select the agent explicitly (no prompt):

```sh
chimera register --agent codex --project /absolute/path/to/project
chimera register --agent claude --project /absolute/path/to/project
chimera register --agent opencode --project /absolute/path/to/project
```

`--project` accepts relative paths and overrides `CHIMERA_PROJECT`. If neither is
set, Chimera uses the current directory. Non-interactive runs require `--agent`.

- Codex uses its global `mcp add` command and honors `CHIMERA_CODEX_BIN`. Running
  it again points that global registration at the selected project.
- Claude Code uses `mcp add` with local scope in the selected project and honors
  `CHIMERA_CLAUDE_BIN`.
- OpenCode adds the entry to the project's `opencode.json` or `opencode.jsonc`,
  keeping existing settings and comments. Chimera reports an error if both
  files exist, the config is invalid, or another `chimera` entry is present
  with different settings.

Run `chimera register --help` for options.

Register Chimera in Codex with this command.

```sh
codex mcp add chimera -- /absolute/path/to/Chimera/target/release/chimera --project /absolute/path/to/project
```

To register Chimera in Claude Code, run this command from the target project.

```sh
claude mcp add --scope local --transport stdio chimera -- \
  /absolute/path/to/Chimera/target/release/chimera --project /absolute/path/to/project
```

To register Chimera manually in OpenCode 2, merge this entry into the project's `opencode.json` or `opencode.jsonc`.

```json
{
  "mcp": {
    "servers": {
      "chimera": {
        "type": "local",
        "command": ["/absolute/path/to/Chimera/target/release/chimera", "--project", "/absolute/path/to/project"]
      }
    }
  }
}
```

For another MCP client, use the absolute path to the binary as the stdio command and pass the same `--project` arguments. You can set `CHIMERA_PROJECT` in the server environment instead of passing `--project`. Set `CHIMERA_CODEX_BIN`, `CHIMERA_CLAUDE_BIN`, or `CHIMERA_OPENCODE_BIN` to an absolute executable path if its harness isn't on the server's `PATH`.

Start a new client session after registering the server. Try asking it to list recent chats from the other harness for this project.

## Tools

| Tool | What it does |
| --- | --- |
| `list_sessions` | Lists recent session IDs and titles from `harness: "claude"`, `"codex"`, or `"opencode"`. Use `archived: true` to list archived Codex sessions. |
| `read_session` | Reads one page of a session by ID. Follow `nextCursor` while it is present, even when a page is empty. Claude Code pages start at the beginning. Codex and OpenCode pages start at the end. Messages within each page appear in chronological order. |
| `delegate_task` | Sends a self-contained prompt to a fresh read-only agent and returns its answer and session ID. One delegation runs at a time per server. The default timeout is 45 seconds and the tool accepts up to 180 seconds, subject to the MCP client’s own timeout. |

Try “Open the most recent Claude Code chat for this project and summarize the decisions.” For delegation, try “Ask Codex to read this project and explain where its configuration is loaded.” The delegate returns findings to the calling agent.

## Scope and authentication

History calls use only persisted sessions whose recorded working directory matches the configured project. Configure a worktree separately to see its chats. History returns user and assistant text, omits tool calls and results, and clips long messages. Chimera does not send history to the destination harness when delegating. The calling agent chooses what to include in the task.

Delegation uses the destination harness's model provider and can incur charges. Authenticate the destination harness or configure its provider credentials in the server environment. Claude Code runs in restricted mode with `Read`, `Glob`, and `Grep` confined to its working directory. OpenCode uses session permissions that allow `read`, `glob`, and `grep` and deny other actions. Codex runs with a read-only sandbox and approval requests disabled. Its sandbox can read files elsewhere on the machine. Use a trusted checkout and review what you share in the prompt.

The Claude Code delegate skips local Claude settings and MCP servers. The Codex delegate honors model configuration and disables hooks, plugins, apps, MCP tools, and web search for the delegated turn. Session history comes from the local harness stores, so chats that exist only in a cloud account will not appear.

## Verify

Run `cargo test --locked` to check MCP discovery, history pagination, and project isolation with local fixtures. The tests run without harness credentials.

## Continuous integration

GitHub Actions runs on pushes, pull requests, manual dispatch, and a weekly schedule. It checks formatting, Clippy, documentation, workflow syntax, and the lockfile against RustSec advisories. It tests the declared minimum Rust version of 1.88 and builds and tests stable Rust on Linux, macOS, and Windows. The MCP integration test uses local fixtures and runs without provider credentials.

Each platform uploads a release binary with this README and a SHA-256 checksum as a workflow artifact, retained for 14 days. The `CI passed` check requires every job to succeed and can be selected as a required branch protection check. Dependabot opens weekly updates for Cargo dependencies and pinned GitHub Actions.

[Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference) · [Codex App Server reference](https://developers.openai.com/codex/app-server) · [OpenCode API reference](https://opencode.ai/docs/api)
