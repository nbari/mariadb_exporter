# Security Policy

## Supported Versions

Security updates are provided for the latest release line only. Older lines
receive fixes by upgrading to the current release.

| Version | Supported          |
| ------- | ------------------ |
| 0.8.x   | :white_check_mark: |
| < 0.8   | :x:                |

## Reporting a Vulnerability

Please do not open a public issue for security vulnerabilities.

Report privately using either channel:

- **Preferred:** GitHub's private vulnerability reporting — open
  [Security → Report a vulnerability](https://github.com/nbari/mariadb_exporter/security/advisories/new).
- **Email:** [nbari@tequila.io](mailto:nbari@tequila.io)

Include, when possible:

- A description of the vulnerability and its potential impact
- Steps to reproduce the issue or a proof of concept
- Affected versions
- Any suggested mitigation or fix

You can expect an initial response within 48 hours and a status update within
seven days. The time required for a fix will depend on the vulnerability's
severity and complexity.

Please allow time for a fix to be released before publicly disclosing the
vulnerability.

## Dependency Auditing

The dependency tree is scanned with
[`cargo-audit`](https://github.com/rustsec/rustsec) against the
[RustSec advisory database](https://rustsec.org) by the
[Security Audit workflow](workflows/security-audit.yml). It runs daily, on
manual dispatch, and whenever `Cargo.toml` or `Cargo.lock` changes. The build
fails on any reported vulnerability, unsound crate, or yanked crate.

To reproduce locally:

```bash
cargo install cargo-audit --locked
cargo audit --deny unsound --deny yanked
```
