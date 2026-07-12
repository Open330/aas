# Changelog

All notable user-facing changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use semantic versioning.

## [Unreleased]

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

[Unreleased]: https://github.com/Open330/aas/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/Open330/aas/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/Open330/aas/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/Open330/aas/releases/tag/v0.1.4
