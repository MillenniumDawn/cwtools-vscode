# Security Policy

## Supported versions

Only the latest published release gets fixes. Older versions are not patched.

## Reporting a vulnerability

Use [private vulnerability reporting](https://github.com/MillenniumDawn/cwtools-vscode/security/advisories/new). That opens a private thread with the maintainers. Please don't file a public issue for a security problem.

Useful things to include: affected version or commit, steps to reproduce, and what an attacker actually gets. A proof of concept helps but isn't required.

This is a hobby project, so response times are best effort. If the report holds up we'll fix it and credit you in the advisory, unless you'd rather stay anonymous.

## Scope

This repo is both halves of CWTools: the TypeScript extension under [`extension/`](extension/), the build and packaging, and the Rust language engine under [`engine/`](engine/). Report anything in either one here.

The extension parses untrusted mod files and fetches a pinned rules commit over the network, so parser crashes, escaping the workspace directory, and anything that gets code executing from a mod file are all worth reporting. The bundled fallback pins live in [`extension/src/host/games.ts`](extension/src/host/games.ts); the reviewed runtime pin set is [`rules-pins.json`](rules-pins.json). The manifest can only replace a known game's full commit SHA, and both pin sets move through a reviewed PR, so an upstream rules repo can't push content into an install on its own.

The server takes the same untrusted mod files from whatever editor drives it, so panics and hangs on malformed input, reading outside the workspace, and anything that gets code executing from a parsed file are worth reporting too.
