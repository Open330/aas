# Security Policy

aas handles provider credentials and encrypted credential bundles. Please report suspected
vulnerabilities privately and never attach real tokens, auth files, keychain exports, or vault
passphrases to a public issue.

## Supported versions

The latest published release receives security fixes. Older releases may be asked to upgrade
before a report is investigated. The `main` branch is development code and is not a supported
release until it has passed CI and been tagged.

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/Open330/aas/security/advisories/new).
Include the affected version and platform, impact, minimal reproduction, and any suggested
mitigation. Redact credentials and other personal data.

We aim to acknowledge reports within 3 business days, provide an initial assessment within 7
business days, and coordinate disclosure after a fix is available. Timelines may vary with scope.

If a credential may have been exposed, revoke or rotate it immediately with the provider; do not
wait for the software investigation to finish.

## Security boundaries

- aas stores credentials locally in provider-compatible files protected with restrictive
  permissions, or in the macOS Keychain where supported.
- Plain `export --all` output contains credentials and must be treated as a secret. Prefer
  `export --all --vault` for data written to disk or transferred between hosts.
- Checksums detect corrupted release downloads. They do not make an untrusted GitHub account or
  compromised host trustworthy.
- The local proxy binds to loopback and authenticates each run with an ephemeral token; it is not
  intended to be exposed to a network.
