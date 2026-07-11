# Contributing

Thanks for improving aas. Bug reports and focused pull requests are welcome.

## Before opening an issue

- Search existing issues and test the latest published release.
- Include `aas --version`, operating system, architecture, provider, expected behavior, and a
  minimal reproduction.
- Remove credentials, account identifiers, auth files, and private usage data from logs.
- Report vulnerabilities through [SECURITY.md](SECURITY.md), not a public issue.

## Development workflow

```bash
git clone https://github.com/Open330/aas.git
cd aas
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked
cargo deny check advisories bans licenses sources
```

For macOS app changes:

```bash
cd apps/aas-bar
swift test -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
swift build -c release -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors
```

Keep changes scoped, add tests for behavior changes, update README/design documentation when the
CLI contract changes, and do not commit generated build output or credentials. Maintainers handle
version tags and releases after all required checks pass.

By participating, you agree to follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
