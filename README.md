# aas — Agent Account Switcher

A single-binary, dependency-free **multi-account switcher for LLM coding agents**
(Claude Code, Codex, Grok/xAI, Z.AI, Cursor). Rust rewrite of
[`asx`](https://github.com/enif-lee/asx).

- Store each account's credential in its own `0600` file / OS keychain entry and switch instantly.
- Run one-off, profile-scoped agent sessions without touching your default login.
- Cross-provider execution: run one agent's UI against another provider's backend (ASX Proxy).
- Live usage at a glance (`aas list -u`) — fetched in parallel, rendered as a table.
- **Reads existing `asx` state** — drop-in adoption, usually zero re-login.

## Status

Functionally complete port (P1–P4): storage/keychain/import, all provider adapters + parallel
`list -u`, every CLI command, same- and cross-provider `exec`, and the translating proxy.
**90 tests** across the workspace. See [`docs/DESIGN.md`](docs/DESIGN.md) and
[`docs/PARITY_SPEC.md`](docs/PARITY_SPEC.md).

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

## Develop

```bash
cargo build
cargo test
```

## License

MIT
