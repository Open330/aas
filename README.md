<div align="center"><img src="https://quickstart-for-agents.vercel.app/api/header.svg?theme=opencode&logo=aas&title=Agent+Account+Switcher" width="640" /></div>

# aas — Agent Account Switcher

A single-binary, dependency-free **multi-account switcher for LLM coding agents**
(Claude Code, Codex, Grok/xAI, Z.AI, Cursor). Rust rewrite of
[`asx`](https://github.com/enif-lee/asx).

- Store each account's credential in its own `0600` file / OS keychain entry and switch instantly.
- Run one-off, profile-scoped agent sessions without touching your default login.
- Cross-provider execution: run one agent's UI against another provider's backend (ASX Proxy).
- Live usage at a glance (`aas usage`) — every account fetched in parallel, rendered as a table.
- **Reads existing `asx` state** — drop-in adoption, usually zero re-login.

## Quickstart for Agents

Paste this into your coding agent (Claude Code, Codex, …) to install and set up `aas`:

<div><img src="https://quickstart-for-agents.vercel.app/api/header.svg?theme=claude-code&title=Set+up+aas&lang=Agents" width="100%" /></div>

```
Install the aas CLI (Agent Account Switcher) from https://github.com/Open330/aas

1. macOS/Linux — run:  curl -fsSL https://raw.githubusercontent.com/open330/aas/main/install.sh | sh
   (Windows PowerShell:  irm https://raw.githubusercontent.com/open330/aas/main/install.ps1 | iex)
2. Verify it works:     aas --version
3. Show my accounts and live usage:  aas list  &&  aas usage

aas reads my existing asx state, so my current logins should already appear.
```

<div><img src="https://quickstart-for-agents.vercel.app/api/footer.svg?theme=claude-code&tokens=1.8k&model=Opus+4.8" /></div>

## Install

macOS / Linux (single static binary — no Node, no runtime):

```bash
curl -fsSL https://raw.githubusercontent.com/open330/aas/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/open330/aas/main/install.ps1 | iex
```

From source:

```bash
cargo install --path crates/aas-cli --locked   # -> ~/.cargo/bin/aas
```

## Quick start

```bash
# Add accounts (each stored as its own isolated profile)
aas login claude work
aas login codex personal              # opens a browser
aas login codex server --headless     # CLI-only box: device-code flow (no browser)

# See what you have, and live quota for every account (parallel fetch)
aas list
aas usage

# Make a stored account the active one (writes the provider's native login)
aas switch codex personal

# Run the native agent under a profile, without changing your default login
aas exec work -- --version

# Cross-provider: run Claude's UI on the codex backend (via the local proxy)
aas exec personal.codex claude

# Use a profile in the *current shell* without switching your default
eval "$(aas export personal.codex)"       # POSIX (bash/zsh)
aas export zai work                        # prints: export ZAI_API_KEY="…"
aas export codex work --shell fish | source          # fish
aas export codex work --shell powershell | iex       # PowerShell

# Adopt / inspect existing asx state (usually a no-op — aas reads the same files)
aas import
```

`switch` vs `exec` vs `export`:

- **`switch <name>`** writes the stored credential to the provider's native location
  (`~/.codex/auth.json`, Claude keychain, …) so running `codex`/`claude` directly uses it.
- **`exec <name>`** runs the agent under a profile-scoped home without touching your default.
- **`export <name>`** prints the env (`CODEX_HOME=…`, `ZAI_API_KEY=…`, …) to activate a
  profile in the current shell only.

`load` is different: it snapshots the **currently logged-in** native credential into a profile
(`aas load codex`), rather than activating a stored one.

## Commands

| Command | Description |
|---|---|
| `list [provider]` (alias `ls`) `-u`,`-d` | List accounts per provider. `-u` shows the live usage table; `-d` dumps stored credentials. |
| `usage [provider]` (alias `u`) | Live usage table for every account (shorthand for `list -u`). |
| `status [provider]` | Show the active account per provider. |
| `login [provider] [name]` `--long-lived`, `--device-auth`/`--headless`, *share flags* | Login and store a new **isolated** profile. `--long-lived` uses Claude's `setup-token`; `--device-auth` uses a browserless device-code flow. |
| `load [provider] [name]` | Snapshot the **currently logged-in** credential as a **system** profile (auto-scans providers if none given). |
| `switch <provider> <name>` (alias `s`) | Make a stored account the active credential. |
| `exec <name> [target] [args…]` (alias `e`) | Run the native CLI under a profile. If `target` ≠ the profile's provider, requests route through the local **ASX Proxy** (cross-provider). `-b` full-access bypass; cross-run share flags `-s/-i/--share/--isolate/--keep-context`; `--` passes the rest to the agent. |
| `export <name>` `--shell posix\|fish\|powershell` | Print shell env to use a profile in the current shell: `eval "$(aas export <name>)"`. |
| `sharing <name>` *share flags* | Show or change which state (sessions/skills/agents/hooks/settings) an isolated profile shares from the provider's home. |
| `rename <from> <to>` | Rename an account (moves its profile home + markers). |
| `remove [provider] <name>` (alias `rm`) | Remove a stored account. |
| `refresh <provider> <name>` `--no-login` | Rotate a credential via its refresh token (falls back to login unless `--no-login`). |
| `proxy <name> <frontend>` | Start a standalone ASX Proxy for `<name>`'s backend and print env to point a `<frontend>` agent at it. |
| `import` | Inspect/adopt existing `asx` state (usually a no-op — `aas` reads the same files). |

**Share flags** (for `login` / `sharing`, and per-run on cross-provider `exec`): `--shared`
(default), `--isolated`, `--share <a,b,…>`, `--isolate <a,b,…>` over the categories
`sessions, skills, agents, hooks, settings`.

**Providers:** `claude`, `codex`, `grok` (alias `xai`), `zai`, `cursor`.

Colors respect `NO_COLOR` and only apply on a TTY.

## Status

Functionally complete port of `asx` (P1–P5): storage/keychain/import, all provider adapters +
parallel `usage`, every CLI command, same- and cross-provider `exec`, the translating proxy,
and static-binary releases. **90 tests** across the workspace. See
[`docs/DESIGN.md`](docs/DESIGN.md) and [`docs/PARITY_SPEC.md`](docs/PARITY_SPEC.md).

## Develop

```bash
cargo build
cargo test
```

## License

MIT
