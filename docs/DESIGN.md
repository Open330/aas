# aas — Agent Account Switcher (Rust) — Design Document

> Status: **IMPLEMENTED** — Updated for aas v0.1.6. `PARITY_SPEC.md` remains the contract for
> inherited `asx` behavior; aas-only extensions are identified explicitly.

`aas` is a from-scratch Rust rewrite of [`asx`](https://github.com/enif-lee/asx), a
multi-account switcher for LLM coding agents (Claude Code, Codex, Grok/xAI, Z.AI, Cursor).
It keeps each account's credential in its own `0600` file / OS keychain entry and switches
the active login instantly, runs one-off profile-scoped agent sessions, and proxies one
agent's UI onto another provider's backend.

- **Repo:** `github.com/open330/aas`
- **Binary:** `aas`
- **Source of truth for behavior parity:** the `asx` TypeScript implementation (v0.3.0).

---

## 1. Goals & Non-Goals

### Goals
1. **Full behavioral parity** with `asx` v0.3.0 — every command, flag, provider, and the
   cross-provider proxy.
2. **Single static binary, zero runtime.** `curl | sh` drops one executable. No Node, no
   nvm bootstrap. This is the primary motivation for the rewrite.
3. **Fast startup** (~2ms vs Node's ~100ms) for a frequently-invoked switcher.
4. **Native async** for parallel usage fetches (`aas list -u` fans out all accounts at once).
5. **Read existing `asx` state** — import/adopt `asx`'s `accounts.json`, profile homes, and
   keychain entries so current users migrate with zero re-login where possible.
6. **A modern, polished CLI** — tables, aligned meters, color, spinners.

### Non-Goals
- No hosted account service or credential broker; credentials remain local to the user's host.
- The optional macOS app and BarShelf widget are presentation layers; the scriptable CLI and its
  JSON contract remain the engine.
- No change to native credential formats or provider wire protocols — aas remains
  byte-compatible with the native CLIs (`claude`/`codex`/`grok`) and with `asx`'s stores.

### Why Rust for *this* tool (honest framing)
The runtime is **I/O-bound** (network for usage, subprocess for login/exec) — raw compute
speed is not the win. The real, structural wins are: **(1)** a dependency-free static binary
(kills the Node-LTS bootstrap in `asx`'s installer), **(2)** near-zero startup latency, and
**(3)** first-class async concurrency for the parallel-usage feature. Choose it for these,
not for "Rust is faster at running the logic."

---

## 2. Crate Stack

| Concern | `asx` (TS) | `aas` (Rust) |
|---|---|---|
| CLI parsing | commander | **clap** (derive) |
| Async runtime | node | **tokio** |
| HTTP client | fetch | **reqwest** (rustls) |
| HTTP server (proxy) | node http | **axum** (hyper/tower) + SSE streaming |
| JSON / schema | zod | **serde** + **serde_json** (typed structs) |
| Tables | cli-table3 | **comfy-table** |
| Colors | chalk | **owo-colors** (+ **anstream** for auto strip on non-TTY) |
| Interactive prompts | @inquirer/prompts | **dialoguer** (or **inquire**) |
| Spinners/progress | — | **indicatif** |
| JWT claim decode | custom base64 | **base64** + serde (no verification) |
| Keychain (macOS) | `security` CLI | **security-framework** (fallback: shell `security`) |
| Keychain (Linux/Win) | `.credentials.json` file | file (same scheme as `asx`) |
| Hashing (keychain svc name) | node:crypto sha256 | **sha2** |
| Time / reset formatting | Date | **time** / **chrono** |
| Error handling | throw | **anyhow** (app) + **thiserror** (libs) |
| Temp dirs (cross-run home) | fs | **tempfile** (or explicit, for `--keep-context`) |

Rationale notes:
- **rustls** (not native-tls) keeps the static-binary promise (no OpenSSL linkage).
- **anstream + owo-colors** give the same "color on TTY, plain when piped" behavior asx got
  from chalk.
- **security-framework** talks to the macOS Keychain directly; if reproducing `asx`'s exact
  service-name scheme via the framework proves fiddly, we fall back to invoking the same
  `/usr/bin/security` argv `asx` uses (documented in §Keychain) for guaranteed compatibility.

---

## 3. Repository Layout

A Cargo **workspace** so the proxy (highest-risk, most code) is an isolable crate with its
own test surface, and the account/provider core can be reused.

```
aas/
├─ Cargo.toml                # workspace
├─ crates/
│  ├─ aas-cli/               # bin: clap commands, output rendering (tables/bars)
│  ├─ aas-core/              # accounts store, profile homes, secure store, platform paths
│  ├─ aas-providers/         # provider adapters (claude/codex/grok/zai/cursor)
│  ├─ aas-proxy/             # cross-provider translating HTTP server (axum) + adapters
│  └─ aas-import/            # read/adopt existing `asx` state (accounts.json, homes, keychain)
├─ apps/aas-bar/             # optional native macOS usage menubar app
├─ widgets/                  # optional BarShelf usage widget
├─ docs/
│  └─ DESIGN.md              # this file
├─ install.sh / install.ps1  # binary-drop installers (no Node)
└─ .github/workflows/        # cross-platform release (static binaries)
```

Module boundaries mirror `asx`'s directories (`providers/`, `storage/`, `proxy/`, `utils/`)
to make the port a near 1:1 translation and keep diffs reviewable against the TS source.

---

## 4. Compatibility & `asx` Import Strategy

The design constraint: **an existing `asx` user runs `aas` and their accounts just work.**

`aas` will default to the **same on-disk locations** `asx` uses (so no migration needed at
all in the common case), and additionally provide an explicit adopt/import path:

- **Config/data dir:** default to `asx`'s dir (`~/Library/Application Support/asx/` on macOS;
  XDG on Linux) so `accounts.json` and `profiles/<provider>-<name>/` are shared. A distinct
  `AAS_*`/`aas/` dir is opt-in via env for a clean-slate install. *(Exact paths + env
  overrides: finalized in §Data Model / §Platform Paths from the codebase mapping.)*
- **accounts.json:** read the existing `version: 1` schema verbatim (serde struct matching
  `AccountRecord`). Write back in the same shape so `asx` and `aas` can coexist during
  migration.
- **Profile homes:** reuse `profiles/<safeProfileDirName(provider,name)>/` unchanged — the
  native auth files (`auth.json`, `.credentials.json`) inside stay put. Because the legacy
  sanitizer is not injective, every add/import/rename reserves the logical name under the
  account-store lock and rejects a second name that resolves to the same home.
- **macOS Keychain:** reproduce `asx`'s exact service-name derivation
  (`Claude Code-credentials-<sha256(CLAUDE_CONFIG_DIR)[:8]>`) so stored Claude credentials
  are found without re-login. *(Exact scheme + `security` argv: from §Keychain mapping.)*
- **`aas import` command (aas-import crate):** with no file, validates and reports the shared
  `asx` state. With a file or stdin, restores an aas portable credential bundle. Bundles can be
  plaintext JSON or passphrase-encrypted age/scrypt files; encrypted input is auto-detected.

Guiding rule: **prefer zero-migration adoption** (same paths, same formats) over a copy step;
the `import` command is the safety net, not the primary flow.

---

## 5. Command Reference (parity target)

> Filled from the CLI/exec/sharing mapping. Each command below reproduces `asx` semantics
> exactly (positional args, flags, aliases, branch behavior).

Full per-command behavior (args, flags, every branch) is in [`PARITY_SPEC.md`](./PARITY_SPEC.md) §4–§5.
Summary:

| Command | Alias | Purpose |
|---|---|---|
| `list [provider\|account]` `-u -d --sort name\|added\|stored` | `ls` | Filterable account list; `-u` live usage bars (parallel), `-d` dump creds. Default account order is case-insensitive name within fixed provider order. |
| `usage [provider\|account]` `--json --sort name\|added\|stored` | `u` | Live usage for all matching accounts; `--json` is the stable app/widget integration contract. |
| `load [provider] [name]` | | Snapshot the live system credential as a **system** profile (auto-scans all providers if none given; email-dedup). Rejects share flags. |
| `login [provider] [name]` `--long-lived` +share | | Fresh native login into an **isolated** profile home; `--long-lived` = Claude `setup-token`. |
| `switch <provider> <name>` or `switch <account>` | `s` | Write a stored profile back to the provider's live store. |
| `status [provider]` | | Show asx-tracked active account per provider. |
| `rename <from> <to>` | | Move profile home + update metadata + active markers. |
| `remove [provider] <name>` | `rm` | Delete account + its secret/home. |
| `exec <name> [target] [args…]` | `e` | Run the native CLI under a profile. `target`≠provider → cross-provider via proxy. `-b` bypass, `-d` debug, cross opts `-s/-i/--share/--isolate/--keep-context`, `--` passthrough. |
| `sharing <name>` +share | | Show/set which categories an isolated profile shares. |
| `refresh <provider> <name>` or `refresh <account>` `--no-login` | | Rotate credential via refresh token; falls back to login unless `--no-login`. |
| `proxy <name> <frontend>` | | Standalone proxy; prints env/config to point `<frontend>` at the profile's backend. |
| `export [name]` / `export --all [--vault]` | | Print profile environment or export every account and credential as a portable JSON/encrypted bundle. |
| `import [file|-]` | | Inspect shared `asx` state or restore a portable JSON/encrypted bundle (see §4). |

A bare `asx <account> …` (unknown subcommand that resolves to an account) is shimmed to
`exec` — replicate this default-command behavior.

Two fixes already made in the `asx` TS source that must be carried into `aas`:
- `list` hides providers that have **no** accounts (only shows an empty section when a
  provider is named explicitly, e.g. `aas list zai`).
- Codex login uses `codex login` (not bare `codex`) so OAuth completes and writes `auth.json`.

---

## 6. Data Model

All on-disk formats are **inherited from `asx` unchanged** (see §4). Full field-level and
operation-level detail is in [`PARITY_SPEC.md`](./PARITY_SPEC.md); the essentials:

**`<config>/accounts.json`** (`version: 1`):
```jsonc
{ "version": 1, "accounts": [ {
  "provider": "claude",          // 'claude'|'codex'|'grok'|'zai'|'cursor'
  "name": "e-ed@callabo",        // GLOBALLY UNIQUE across providers
  "label": "e-ed@callabo",
  "email": "e-ed@callabo.ai",
  "addedAt": "2026-07-06T...Z",
  "profileType": "isolated",     // 'system' | 'isolated' (absent on legacy)
  "share": ["sessions","skills"] // absent = share all, [] = fully isolated
} ] }
```
Written pretty, `0600`, through an fsynced same-directory temp file and atomic replacement while
holding a cross-process lock; malformed/version-incompatible stores fail closed and a last-valid
backup is retained. `add_account` enforces **global name uniqueness** (same name under a
different provider → error) plus unique resolved profile homes. This is exactly the constraint we hit during migration
(`e-ed@callabo` claude vs codex) — keep it, and keep `asx`'s provider-scoped auto-name
`deriveAccountName` = `<email-local>.<providerShort>` (e.g. `e-ed.codex`) to avoid collisions.

**Active marker** — a **separate** `<config>/.active.json`: `{ "<provider>": "<name>", "updated": "<iso>" }`
(one key per provider, lowercased provider). Not inside `accounts.json`.

**Profile home** — `<config>/profiles/<safeProfileDirName>` where
`safeProfileDirName(provider,name) = "{normKey}-{name}"` with `[^A-Za-z0-9_.-] → _`
(normKey: contains `claude`→`claude`, `xai`→`grok`). Native cred file inside:
claude `.credentials.json`, codex/grok `auth.json`, else `credential`.
The sanitizer stays byte-compatible with asx, so aas rejects any logical-name pair that maps to
the same result and commits account identity before credential creation.

Rust: model as serde structs; `Store { version, accounts: Vec<AccountRecord> }`,
`AccountRecord { provider, name, label: Option, email: Option, added_at, profile_type:
Option<ProfileType>, share: Option<Vec<Category>>, .. }`. Note `asx` mixes exact-string and
lowercased-provider matching across ops — replicate per-op (documented in PARITY_SPEC §4).

The array order remains meaningful and can be requested with `--sort stored`. Display commands
default to the provider registry order (`claude`, `codex`, `zai`, `grok`, `cursor`) and then a
case-insensitive account-name order. `--sort added` uses `addedAt` oldest-first. Sorting never
rewrites `accounts.json`; it only controls the returned/rendered view.

**Portable bundles:** `export --all` serializes account metadata plus aas-managed provider
credentials. `--vault` wraps that JSON in the standard age passphrase format (scrypt recipient +
authenticated encryption). Passphrases are read without echo or from the short-lived
`AAS_VAULT_PASSPHRASE` environment variable. Imports auto-detect encrypted input and do not
include browser cookies, conversation history, or machine-specific active markers.

## 7. Provider Adapters

Trait `ProviderAdapter` mirrors `asx`'s interface: `load_current`, `switch_to`,
`current_email`, `usage`, `clear_current`, `login_command`, `current_credential`,
`is_expired`, `refresh`, `load_long_lived_token`, `login`. Registry keys:
`claude`/`claude-code`(alias)→claude, `codex`, `grok`, `zai`, `cursor`.

**Key design change — structured usage.** `asx`'s `getUsage` returns a preformatted,
color-embedded multi-line **string**. `aas` splits this:
```rust
struct Usage { headline: String, plan: Option<String>, meters: Vec<Meter>, notes: Vec<String>, error: Option<String> }
struct Meter { label: String, used_pct: f64, reset: Option<SystemTime> }  // label: "5h" | "7d" | "credits" ...
```
The adapter returns data; the CLI renders bars/tables/colors. This (a) enables the
**parallel-fetch + single-render** goal, and (b) makes the table layout provider-agnostic
(claude/codex → 5h+7d meters; grok → credits meter + rate-limit notes; zai → 5h meter;
cursor → note only).

Exact endpoints/headers/fields per provider are in PARITY_SPEC §2 (reproduce verbatim — the
wire contracts must not drift). Highlights / gotchas to carry over:
- **Claude:** `GET api.anthropic.com/api/oauth/usage` + `/api/oauth/profile`, headers
  `Authorization: Bearer`, `anthropic-version: 2023-06-01`, `anthropic-beta: oauth-2025-04-20`.
  Refresh via `POST console.anthropic.com/v1/oauth/token` (client_id
  `9d1c250a-e61b-44d9-88ed-5944d1962f5e`). On 401/403 usage does **not** fall back to stale data.
- **Codex:** `GET chatgpt.com/backend-api/wham/usage` (headers incl. `ChatGPT-Account-Id`,
  `User-Agent: codex-cli`). Refresh is the **`codex doctor --summary` trick** — shell out with
  `CODEX_HOME=<profile home>` so the native CLI rotates `auth.json` in place (no HTTP refresh).
- **Grok:** JWT-vs-apikey branch → `cli-chat-proxy.grok.com/v1/billing`+`/settings` (subscription)
  or `api.x.ai/v1/api-key` (credits) + rate-limit headers off `/models` (or a probe call).
- **Z.AI:** `GET api.z.ai/api/monitor/usage/quota/limit` — **`Authorization: <raw key>` (NO `Bearer`)**,
  unlike the key-test endpoint which uses `Bearer`. Easy to get wrong.
- **Expiry skew** is `+60s` for both claude and codex.

## 8. Keychain & Platform Paths

**macOS Claude Keychain** (the migration-critical part): service name =
`"Claude Code-credentials"` when no config dir, else
`"Claude Code-credentials-" + hex(sha256(configDir))[..8]`. Account = `os user || $USER || "user"`.
Reproduce with `sha2` + hex, first 8 chars. Access via the `security` CLI (identical argv, for
byte-compatibility with existing entries):
```
security find-generic-password   -s <service> -a <user> -w
security add-generic-password    -s <service> -a <user> -w <raw> -U
security delete-generic-password -s <service> -a <user>
```
(We may use `security-framework` later, but the CLI argv is the compatibility contract.)
**Gotcha:** the hash input differs by context — live reads hash `getClaudeConfigDir()`
(`CLAUDE_CONFIG_DIR` or `~/.claude`); the per-account vault hashes `getProfileHome(provider,name)`.
Two different services. PARITY_SPEC §5 has both.

Non-macOS (and keychain-less): `.credentials.json` file — live at `<claudeConfigDir>/.credentials.json`,
per-profile at `<profileHome>/.credentials.json`.

**Path roots & env overrides** (default to `asx`'s locations for zero-migration adoption):
`<configBase>` = win `%APPDATA%` · macOS `~/Library/Application Support` · linux `$XDG_CONFIG_HOME|~/.config`;
`<config>` = `<configBase>/asx`; profiles = `<config>/profiles`. Provider homes honor
`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` (with `~` expansion); system homes `~/.claude`,
`~/.codex`, `~/.grok`. `aas` reads these same vars so it drives the same native CLIs.

## 9. Proxy (cross-provider)

Exhaustive wire-level spec in [`PARITY_SPEC.md`](./PARITY_SPEC.md) §H. Architecture:

A short-lived, loopback-only HTTP server bound per `exec`/`proxy` session, with a
**hub-and-spoke transcoder**:
```
frontend wire ─[agent.parse_request]─▶ COMMON ─[backend.build_request]─▶ upstream HTTP
upstream SSE  ─[backend.parse_chunk]──▶ COMMON events ─[agent.format_chunk]─▶ frontend SSE
```
- **`AgentAdapter`** (frontend = the CLI talking to us): `parse_request`, `stream_headers`,
  `format_stream_chunk`, `format_response`, `format_models`.
- **`BackendAdapter`** (upstream we call, from the profile): `build_request`, `parse_stream_chunk`,
  optional `is_retryable`. Adapters: claude/codex/grok are both frontend+backend; **zai is
  backend-only**.
- **COMMON IR** (`CommonRequest`/`CommonMessage`/`CommonToolCall`/`CommonEvent`/`CommonResponse`)
  — the only thing each adapter needs to know besides its own wire. Tool-call args are kept as
  raw JSON **strings** to round-trip losslessly.

Rust mapping: **axum** on hyper/tokio. Server = a `TcpListener` bound to `127.0.0.1:0` (keep it —
avoids `asx`'s free-port TOCTOU race) and requires a random per-run token on every route.
Inference bodies are capped at 16 MiB and at most 32 requests may be in flight. Streaming uses
`reqwest`'s byte stream → an SSE framer
(split on `\n\n` over the accumulated, CRLF-normalized buffer) → `parse_stream_chunk` →
tokio channel → axum `Sse`/`Body` back to the frontend.

**The hard-won robustness that MUST be reproduced 1:1** (these are the PR #3 behaviors —
`aas` ports the exact semantics, with dedicated tests):
1. Buffer `tool_call_delta`s by wire index; flush merged complete `tool_call`s at `done`/EOF
   **before** the terminator.
2. Stream ends without `done` → emit a synthetic warning text + terminator (unless client gone).
3. Mid-stream upstream error (headers already sent) → flush tools, emit error-text chunk +
   terminator, always cancel the upstream body. Never write raw JSON into an open SSE stream.
4. Client disconnect (`res.on('close')` → in Rust, the axum connection-drop / a `CancellationToken`)
   aborts upstream reads; all writers early-return.
5. Upstream failures preserve their non-2xx status with a structured error body until streaming
   has begun. A mid-stream failure must use the already-open SSE response as described above.
6. Retry: ≤5 attempts; retry on network error (except auth/cert/invalid-url), on
   `{408,429,500,502,503,504}`, and on `backend.is_retryable(status, body)` (z.ai overload codes
   `1301/1302/1304/1305` even at HTTP 200). Never retry `{400,401,403,404,405,410,422}`. Backoff
   `min(30s, 500·2^(n-1)) + rand(0..499)ms`; per-attempt 120s timeout.
7. The `errText` sentinel: a consumed body (error / non-stream) ⇒ treat as failure even at 200;
   an untouched streaming body flows straight through.

Provider-specific transforms to preserve exactly: **no temperature/top_p/top_k to the Claude
backend** and thinking disabled for non-Fable; Codex namespace/subagent tool flattening
(`ns__name` ↔ `{namespace,name}`) so Codex multi-agent works against any backend; z.ai uses
`thinking:{type:enabled|disabled}` (not `reasoning_effort`); grok drops `reasoning_content`
deltas; Claude-frontend model-id `claude-asx-` wrap/unwrap to survive Claude Code's `/model`
picker filter.

**Injection** (`aas-proxy::inject`, per frontend, so the launched binary talks to the proxy and
skips its own auth): codex → write `config.toml` (`model_provider="asx-proxy"`, `base_url=<url>/v1`,
`wire_api="responses"`) + `models.json`, set `CODEX_HOME`/`ASX_PROXY_API_KEY`; claude → env only
(`ANTHROPIC_BASE_URL`, per-run `ANTHROPIC_AUTH_TOKEN`, slot→model remap, drop gateway
discovery); grok → `config.toml` per-model blocks + `GROK_HOME`. All frontends use the same
per-run proxy token. Explicit scratch homes override inherited agent-home variables, and Grok's
`always-approve` mode is emitted only for an explicit `--bypass`. Model registry
(`aas-proxy::models`) resolves picker `id` → real `{model, effort}`, override precedence
`env(ASX_<PROV>_MODELS) > <config>/models.json > defaults`.

---

## 10. Usage Concurrency Model (the `list -u` win)

`aas list -u` collects every `(provider, account)` pair, then runs, concurrently via
`tokio::join_all` / `FuturesUnordered`:
1. `ensure_fresh` (auto-refresh an expired credential),
2. the live-system-credential match (mark "current in system"),
3. the provider `usage()` fetch.

Results are collected, then rendered **once** as a table (no interleaved partial output). A
single `indicatif` spinner covers the whole fan-out. A per-request timeout bounds the slowest
provider. This replaces `asx`'s sequential `await`-per-account loop.

---

## 11. Distribution & CI

- **Static binaries** for macOS (arm64 + x86_64, ideally a universal2), Linux
  (x86_64 + aarch64, musl for portability), Windows (x86_64).
- Release via GitHub Actions matrix (or `cargo-dist`) attaching artifacts to a tagged
  release, mirroring `asx`'s "Publish Release" workflow.
- **`install.sh` / `install.ps1`** detect OS/arch and download the matching binary — **no
  Node/nvm bootstrap** (the key simplification over `asx`).
- Optional: publish to Homebrew tap / `cargo install` later.

---

## 12. Testing Strategy

- Port `asx`'s unit tests (~1,894 LOC, 120 tests) into Rust `#[test]`/`#[tokio::test]`,
  preserving the exact assertions (JWT claims, wire transforms, share-flag resolution,
  usage parsing, keychain service-name derivation).
- **Golden/snapshot tests** (via `insta`) for CLI table output and proxy wire translations.
- **Wiremock**-based tests for provider `usage()` and proxy backends (deterministic HTTP).
- The proxy's streaming/retry behavior gets dedicated tests reproducing the scenarios from
  `asx` PR #3 (stream interruption, client disconnect, z.ai overload retry).

---

## 13. Phased Implementation Plan

TS `asx` remains the shipping version until `aas` reaches parity per phase.

- **P0 — Scaffold:** workspace, crates, clap skeleton, CI + release pipeline for static
  binaries, `.gitignore`, licenses.
- **P1 — Core:** `aas-core` (accounts store, profile homes, secure store + keychain),
  platform paths, `aas-import` (adopt asx). Port core tests.
- **P2 — Single-provider CLI:** `list` (+ parallel `-u` + table), `load`, `login`, `switch`,
  `status`, `rename`, `remove`, `sharing`, `refresh`; providers' auth + `usage()`. → daily-
  driver parity.
- **P3 — exec (same-provider):** home/env injection + sharing symlinks.
- **P4 — Proxy (cross-provider):** `aas-proxy` server + adapters + streaming + tool calls +
  sessions; port proxy tests. Highest risk, done last.
- **P5 — Distribution swap:** binary-drop installers; deprecate/wrap the npm package.

Risk note: P1–P3 (~2,900 LOC equivalent) port cleanly and quickly. **P4 (~1,650 LOC, the
streaming proxy) is the one real hazard** — budget the most time there and port its tests
alongside the code.
