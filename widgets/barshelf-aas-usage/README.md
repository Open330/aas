# aas Usage — BarShelf widget

Live LLM quota for every `aas` account, right in your macOS menubar via
[BarShelf](https://github.com/Open330/barshelf) — a scriptable menubar
widget platform.

The widget runs `aas usage --json` (all accounts fetched in parallel) and
renders the remaining quota per provider/account as a compact table in the
BarShelf popover.

## Install

[![BarShelf Install](https://img.shields.io/badge/BarShelf-Install-0A84FF)](barshelf://install?url=https%3A%2F%2Fgithub.com%2FOpen330%2Faas)

With the `mbk` CLI:

```bash
mbk install https://github.com/Open330/aas
```

Or open the deep link directly (requires BarShelf to be installed):

```text
barshelf://install?url=https%3A%2F%2Fgithub.com%2FOpen330%2Faas
```

## Requirements

- [BarShelf](https://github.com/Open330/barshelf) installed (macOS 13+ on
  Apple Silicon).
- The `aas` binary available — the widget looks in `$AAS_BIN`,
  `~/.local/bin/aas`, `~/bin/aas`, `~/.cargo/bin/aas`, `/opt/homebrew/bin/aas`,
  `/usr/local/bin/aas`, then `PATH`. See the [aas install instructions](../../README.md#install).
- At least one account added (`aas login <provider> <name>`).

## Permissions

The widget only requests permission to execute `aas usage --json`. No network,
file-read, or keychain access is declared in the manifest — all credential
handling stays inside the `aas` binary itself.

## Refresh behavior

Usage is considered for refresh only while the BarShelf popup is open and this
widget is visible. A successful result remains fresh for 10 minutes. The
manifest's `popupOnly` policy explicitly disables interval polling, background
execution, file watchers, deadline/wake refreshes, and event triggers. AAS also shares its own 10-minute success cache
and per-account fetch locks across terminal and widget callers.
