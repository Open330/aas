# Launch copy: changes

> **Draft. Not posted.** What changed since the last release, for people who already use or follow aas.
> Source: [CHANGELOG.md](../../CHANGELOG.md) `[Unreleased]` as of 2026-09-25 (expected v0.1.14).
> Demo: `docs/demo/render.sh --mode changes --channel <readme|social|square>`.

## GitHub release notes

> ### Added
> - `aas exec codex` (and the bare `aas codex`) now runs that provider's active account, so a provider name works wherever an account name does. An account that happens to be named after a provider still wins.
>
> ### Fixed
> - `claude` and `codex` keep following `aas switch` in shells started from an agent session (for example a tmux server). Previously they could stay on the old account for the rest of that session.
>
> Update: `brew upgrade aas`

## X (attach `--mode changes --channel social`)

> aas v0.1.14: `aas exec codex` now runs whichever Codex account is active, and `claude`/`codex` keep following `aas switch` inside tmux and agent-spawned shells.
>
> brew upgrade aas

## LinkedIn (attach `--mode changes --channel square`)

> aas v0.1.14 is out. A provider name now runs its active account (`aas exec codex`), and a fix keeps `claude` and `codex` following `aas switch` in long-lived shells such as tmux.
>
> Update with `brew upgrade aas`. Full notes: https://github.com/Open330/aas/releases
