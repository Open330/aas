---
name: aas
description: Discover registered aas accounts and recover agent CLI work from quota exhaustion or rate limits using another authorized account. Use when running coding agents through aas, selecting an account, or handling a quota or HTTP 429 failure; not for unrelated application rate limits.
---

# Account-aware agent execution

aas may hold multiple accounts. Discover them at runtime; never hard-code local
account names or assume only the active account exists. Use the installed `aas`
binary and its help to check feature availability.

## Discover and choose

Run `aas candidates --provider <provider> --json` before choosing an account.
The versioned JSON contains `accounts`, sorted with eligible accounts first and
then by observed remaining quota. Use `name` as the CLI account argument, not `id`.
`eligible` means observed quota is available; it does not guarantee model access,
valid inference credentials, or compatibility. `cached` and `fetchedAtMs` describe
freshness. Missing meters and usage errors are unknown, not available capacity.
`usageCooldownUntilMs` is a usage-endpoint cooldown, not an inference reset time.
Use `--fresh` to refresh evidence when needed; it still honors usage backoff.
Repeat `--exclude <name>` to exclude accounts already attempted.

On older versions without `candidates`, use `aas list` and `aas usage --json`.
Do not expose credentials: avoid `list -d`, credential files, and `export` output.

Prefer a compatible account on the same provider. Respect the user's account,
organization, model, and data-routing constraints. Registration alone does not
authorize sending a project's data to another organization or provider.

## Execute and recover

For an authorized alternate account, run `aas exec <name> -- <agent arguments>`.
This selects the account for a new process; it does not change a running agent's
login. Prefer this over `aas switch`, which changes the default login.

For automatic recovery, explicitly list allowed fallback accounts:

```bash
aas exec primary.codex --fallback backup.codex -- <agent arguments>
```

Names above are examples: replace them with discovered account names. Repeat
`--fallback <name>` for additional accounts, before `--`. All must use the same
provider and endpoint. This mode runs through the local translating proxy even
for same-provider execution; its model catalog and request handling can differ
from native execution. Proxy backends support Claude, Codex, Grok, Z.AI, and Kimi;
Z.AI/Kimi need an explicit supported frontend, e.g. `aas exec <name> claude ...`.

Automatic fallback applies only to upstream HTTP 429 before response streaming.
The proxy retires that account for the rest of this proxy session and advances
in order. It never restarts the CLI or replays completed tools. Once all accounts
are retired it returns an error; it does not loop or automatically re-enable
accounts. Authentication errors, other HTTP errors, and errors inside a stream
do not cause account switching. A quota error without HTTP 429 requires manual
assessment. After quota recovers, a new invocation creates a new pool.

When handling a failed child CLI manually, inspect partial results and resume
only unfinished work. Do not blindly rerun commits, deployments, or other side
effects. Attempt each authorized account at most once per recovery episode and
stop when candidates are exhausted. Report attempted accounts and the unresolved
error without secrets. For an existing process without proxy fallback, respect
any Retry-After/reset time rather than repeatedly retrying the limited account.

A skill cannot execute recovery if its own model request is blocked. Configure
`--fallback` before starting a session for that case. Do not promise live account
replacement or automatic conversation migration from a skill alone.
