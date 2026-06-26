# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in RocksGraph, please report it privately rather than
opening a public GitHub issue.

**Contact:** austinhan1024@gmail.com

Please include:
- A description of the vulnerability and its potential impact
- Steps to reproduce (a minimal example is ideal)
- The version or commit you tested against

## Response Timeline

- We will acknowledge receipt of your report within **72 hours**.
- We aim to provide an initial assessment (severity, affected versions) within **7 days**.

## Disclosure Policy

We follow coordinated disclosure: please give us a reasonable window — typically **90 days** —
to investigate and release a fix before any public disclosure. We'll keep you updated on
progress throughout, and will credit reporters (unless anonymity is requested) once a fix ships.

## Scope

This policy covers:
- The `rocksgraph` Rust crate itself (this repository)
- Its direct interaction with its embedded [RocksDB](https://github.com/facebook/rocksdb)
  dependency (e.g. encoding, key construction, or query-handling bugs that could lead to data
  corruption, panics on untrusted input, or memory-safety issues)

Vulnerabilities in RocksDB itself, or in other upstream dependencies, should be reported
upstream, but we're happy to help route or track those if reported here.
