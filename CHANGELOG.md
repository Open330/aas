# Changelog

All notable user-facing changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use semantic versioning.

## [Unreleased]

## [0.1.9] - 2026-08-24

### Added

- Added Kimi (alias `moonshot`) as a provider, so `aas exec <kimi-account> claude` runs Claude Code
  against a Kimi backend. `aas login kimi --endpoint <moonshot-ai|kimi-code|moonshot-cn>` selects
  the platform that issued the key, validates the key against it, and records it on the account —
  Kimi's platforms issue keys that are not interchangeable. Only hosts known to the provider table
  are accepted, so a tampered store cannot redirect a credential elsewhere.
- Added live Kimi model discovery against the account's own endpoint, with built-in choices as the
  fallback.
- Release installers now verify the archive's GitHub build-provenance attestation against this
  repository's release workflow. Constrained or offline hosts can set `AAS_SKIP_ATTESTATION=1`,
  which gives up publisher authentication and keeps checksum-only integrity verification.

### Changed

- API-key providers (Grok, Z.AI, Kimi) are now driven by one provider table covering env var names,
  API hosts, auth style, key validation, quota strategy, and login method, replacing per-provider
  branches in every adapter function.
- Backends that already speak the agent's wire protocol are relayed instead of translated: the
  proxy rewrites only the model id and credential and streams the reply back byte for byte. Routing
  an Anthropic request through the COMMON intermediate dropped `cache_control`, thinking blocks, and
  image parts, which silently disabled prompt caching on a per-token-billed backend.
- Rust CI and the release quality gates run on Linux, macOS, and Windows; release smoke tests drive
  both installers against the release candidate, including a rejected checksum.

### Fixed

- A backend error mid-stream now flushes buffered tool calls and terminates the stream once, instead
  of appending a synthetic "ended unexpectedly" terminator after a real error. Non-streaming
  requests answer 502 rather than returning a truncated success body.
- `aas status <provider>` and `aas remove <provider> <name>` now normalize the provider name, so
  aliases resolve the same way they do everywhere else.
- `aas import` exits non-zero when the bundle produced conflicts or failures.
- `install.ps1` compares normalized PATH entries, so a prefix collision no longer skips the PATH
  update.
- The aas-bar usage cache reports a failed write instead of dropping it silently, and
  `build-app.sh` keeps the previous known-good bundle until the staged replacement is signed and
  verified.

### Security

- Keychain reads distinguish an absent item from a locked or unavailable Keychain, so rename and
  import can no longer orphan or overwrite a live credential when the Keychain is unreachable.
- Account removal quarantines the profile home and cleans it up only after metadata commits;
  account and active-marker writes roll back together, so a failed removal cannot leave a
  half-deleted profile.
- Credential mutations run under a provider-wide lifecycle lock, so concurrent commits can no longer
  publish metadata from one login with the secret from another.
- Grok token refresh rejects any stored OIDC issuer other than `https://auth.x.ai` and no longer
  follows redirects.
- `aas login` for an isolated profile authenticates into a throwaway staging home and commits over
  the existing profile only after the provider validates the new credential, so a failed re-login
  leaves the previous session intact.
- `aas exec` scrubs inherited provider credentials and home overrides before injecting the selected
  profile, so an ambient key cannot reach the launched agent.
- `aas export --out` creates its destination `O_EXCL` at mode 0600 and refuses plaintext file export
  on Windows, where owner-only ACLs cannot be guaranteed.
- Account storage rejects names that collide only by case (they break once a bundle reaches a
  case-insensitive filesystem) and terminal control characters in account fields.

## [0.1.8] - 2026-07-15

### Added

- Added Pi as a first-class provider, including native `auth.json` load/switch, isolated profile
  execution, shared state, shell export, and Pi as a cross-provider proxy frontend.
- Added GPT-5.6 Sol/Terra/Luna choices while retaining GPT-5.5 compatibility, plus live per-session
  model discovery from Grok and Z.AI.
- Added Grok OIDC expiry detection and refresh-token rotation, including native auth synchronization
  for system profiles.

### Changed

- Claude Opus/Sonnet/Haiku/Fable aliases now resolve to safe backend effort tiers, including
  low-effort Sol for Haiku instead of preview-gated models.
- Grok model choices now forward their advertised `reasoning_effort`; Codex catalogs expose the
  complete low/medium/high/xhigh/max/ultra effort ladder.
- The BarShelf widget now owns its renderer as a declarative workflow: 5h/7d progress bars show
  quota used, while quota left and time until reset are presented as separate text.

### Fixed

- Handled Anthropic `/v1/messages/count_tokens` locally instead of misrouting it to inference.
- Normalized whole-number float spellings in streamed tool arguments and materialized empty Claude
  `end_turn` responses with `stop_sequence: null`, preventing Codex multi-agent and Claude Task
  subagent deserialization failures.
- Labeled and ordered Codex usage windows from their reported duration instead of assuming
  `primary` always means 5h and `secondary` always means 7d. A weekly-only primary window now
  appears correctly as `7d`, while duration-less legacy responses retain the old fallback.

## [0.1.7] - 2026-07-12

### Added

- Added a shared 10-minute last-known-good usage cache, additive `cached`/`fetchedAtMs` JSON
  provenance, and `aas usage --fresh` for explicit live requests.
- Added per-account cross-process locks that coalesce simultaneous credential refreshes and usage
  fetches from terminals, BarShelf, and editor integrations.

### Changed

- Rate-limit backoff is now checked before OAuth refresh, guaranteeing that a backed-off usage
  request performs no provider calls. Transient failures retain cached meters; authentication
  failures remain explicit and never fall back to stale usage.
- The BarShelf usage widget now declares the `popupOnly` policy and disables interval,
  background, file-watch, deadline/wake, and event-triggered execution.

### Fixed

- Removed synthetic exponential growth from persisted usage backoff. `aas` now honors the
  provider's `Retry-After` (with a 60-second fallback only when absent), preventing one machine
  from showing an hour-long local rate limit after the provider has already recovered.
- Automatic refresh failures are surfaced in usage output instead of being silently discarded.

## [0.1.6] - 2026-07-12

### Fixed

- Prevented macOS `security -i` from silently truncating large Claude OAuth credentials after
  hex encoding. Credentials beyond the safe parser limit now use Claude's owner-only
  `.credentials.json` fallback instead of writing a corrupt Keychain item.
- Preserved credentials created by native Claude login without rewriting an identical scoped
  Keychain item or profile file, avoiding false login failures and Keychain ACL changes.
- Applied the same safe large-credential fallback when switching or refreshing the active Claude
  profile on macOS.

## [0.1.5] - 2026-07-11

### Added

- Deterministic account ordering for `list`, `usage`, JSON integrations, and debug output.
- `--sort name|added|stored`, with case-insensitive account-name order as the default.
- Passphrase-encrypted portable credential bundles via `export --all --vault` and automatic
  encrypted import detection.
- Security reporting, support, contribution, and conduct documentation.

### Changed

- README and design documentation now match the complete CLI surface and clarify the distinction
  between the latest release installer and source builds.
- macOS app and BarShelf widget versions advance with the workspace release version.

### Security

- Documented the narrow `RUSTSEC-2026-0173` policy exception. It is an unmaintained build-time
  proc-macro pulled by the latest `age` release, with no patched version; all other advisories
  remain denied.

## [0.1.4] - 2026-07-10

### Added

- CI and staged five-target release workflows with checksums and attestations.
- BarShelf usage widget and native macOS usage app verification.
- MIT license and dependency policy checks.

### Changed

- Hardened account storage, provider adapters, proxy authentication, retries, installers, and
  portable app packaging.

[Unreleased]: https://github.com/Open330/aas/compare/v0.1.9...HEAD
[0.1.9]: https://github.com/Open330/aas/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/Open330/aas/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/Open330/aas/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/Open330/aas/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/Open330/aas/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/Open330/aas/releases/tag/v0.1.4
