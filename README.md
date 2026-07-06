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

🚧 Early port in progress. See [`docs/DESIGN.md`](docs/DESIGN.md) and
[`docs/PARITY_SPEC.md`](docs/PARITY_SPEC.md).

Phases: **P0** scaffold · **P1** core (storage/keychain/import) · **P2** single-provider CLI ·
**P3** exec · **P4** proxy · **P5** distribution.

## Build

```bash
cargo build
cargo test
```

## License

MIT
