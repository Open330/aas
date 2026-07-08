# aas Usage — MenuBucket widget

Live LLM quota for every `aas` account, right in your macOS menubar via
[MenuBucket](https://github.com/jiunbae/menubucket) — a scriptable menubar
widget platform.

The widget runs `aas usage --json` (all accounts fetched in parallel) and
renders the remaining quota per provider/account as a compact table in the
MenuBucket dropdown.

![aas Usage widget screenshot](docs/screenshot.png)
<!-- TODO: replace with a real screenshot (docs/screenshot.png) -->

## Install

[![MenuBucket Install](https://img.shields.io/badge/MenuBucket-Install-0A84FF)](menubucket://install?url=https%3A%2F%2Fgithub.com%2FOpen330%2Faas)

With the `mbk` CLI:

```bash
mbk install https://github.com/Open330/aas
```

Or open the deep link directly (requires MenuBucket to be installed):

```text
menubucket://install?url=https%3A%2F%2Fgithub.com%2FOpen330%2Faas
```

## Requirements

- [MenuBucket](https://github.com/jiunbae/menubucket) installed.
- The `aas` binary available — the widget looks in `$AAS_BIN`,
  `~/.cargo/bin/aas`, `/opt/homebrew/bin/aas`, `/usr/local/bin/aas`, then
  `PATH`. See the [aas install instructions](../../README.md#install).
- At least one account added (`aas login <provider> <name>`).

## Permissions

The widget only requests permission to execute `aas usage --json`. No network,
file-read, or keychain access is declared in the manifest — all credential
handling stays inside the `aas` binary itself.

## Refresh behavior

Usage is fetched when the bucket is opened and considered stale after 10
minutes; there is no background polling interval.
