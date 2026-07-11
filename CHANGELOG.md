# Changelog

All notable user-facing changes are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use semantic versioning.

## [Unreleased]

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

[Unreleased]: https://github.com/Open330/aas/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/Open330/aas/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/Open330/aas/releases/tag/v0.1.4
