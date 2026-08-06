# Security Policy

## Supported versions

Only the latest published release gets fixes. Older versions are not patched.

## Reporting a vulnerability

Use [private vulnerability reporting](https://github.com/MillenniumDawn/cwtools-vscode/security/advisories/new). That opens a private thread with the maintainers. Please don't file a public issue for a security problem.

Useful things to include: affected version, steps to reproduce, and what an attacker actually gets. A proof of concept helps but isn't required.

This is a hobby project, so response times are best effort. If the report holds up we'll fix it and credit you in the advisory, unless you'd rather stay anonymous.

## Scope

This repo is the VS Code extension: the TypeScript client, the build, and packaging. The language server is Rust and lives in [MillenniumDawn/cwtools](https://github.com/MillenniumDawn/cwtools). Report server bugs there.

The extension parses untrusted mod files and fetches a pinned rules commit over the network, so parser crashes, escaping the workspace directory, and anything that gets code executing from a mod file are all worth reporting. The rules pins live in [`client/extension/games.ts`](client/extension/games.ts) and only move through a reviewed PR, so an upstream rules repo can't push content into an install on its own.
