---
name: fix-issue
description: >-
  Take a GitHub issue number (or URL) for this repo, read the issue and its
  thread, gather the engine/extension context it touches, state the real goal,
  and produce a senior-Rust-developer plan: the simplest correct fix, built on
  helpers that already exist, following this repo's conventions. After approval
  it implements, verifies, and opens the PR. Use whenever the user gives an
  issue number or link and wants it fixed, investigated, planned, or triaged,
  even if they don't say "fix-issue".
argument-hint: <issue-number|issue-url> [--plan-only]
---

# Fix a GitHub issue

Input: `$ARGUMENTS` — an issue number or a GitHub issue URL, optionally followed
by `--plan-only`. If there is no issue number, ask for one with
`AskUserQuestion` rather than guessing from the branch name.

You are working as a senior Rust developer on this codebase. That means the
plan comes before the diff, the diff is the smallest one that fixes the cause,
and every claim in the hand-off is backed by a check that actually ran. The
phases below are in the order that keeps you honest: reproduce before you
explain, explain before you change, verify before you report.

## 1. Fetch the issue and what already happened to it

```bash
gh issue view <n> --json number,title,body,state,labels,url,comments
gh pr list --search "<n>" --state all --json number,title,state,url
git log --oneline --grep "#<n>"
```

If `gh` fails, check `gh auth status` and report that instead of paraphrasing
the issue from memory. Read the comment thread as carefully as the body: the
reporter often narrows or corrects the request there, and maintainers may have
already named the suspected cause.

If the issue is closed, or a PR referencing it is already merged, stop and ask
before doing anything else. The likeliest explanations are a duplicate request
or a regression of an earlier fix, and both change what the right work is.

## 2. Classify where the fix lives

Decide which of these the issue is about, and say so in the plan:

| Area | Where | Start by reading |
| --- | --- | --- |
| Engine | `engine/crates/{parser,rules,index,validation,driver,lsp,cli,cache,info}` | `docs/engine/ARCHITECTURE.md` (crate map, CLI-vs-LSP split, "Adding an error code") |
| Diagnostic code `CWxxx` | the validator that emits it | `docs/engine/ERROR_CODES.md` entry for that code |
| Extension host / webview | `extension/src/host`, `extension/src/webview` | `CLAUDE.md` frontend section, `extension/test/` |
| Tooling | `scripts/`, `tests/scripts/`, `.github/workflows` | `CLAUDE.md` "The Python helpers" |
| Rules | `../cwtools-hoi4-config` (sibling checkout, `CWTOOLS_PROJECTS` overrides) | the `.cwt` file the diagnostic points at |

The last row matters more than it looks. The `.cwt` rules are not bundled, so
a "the validator is wrong about X" report is as likely to be a rules bug as an
engine bug. If the correct fix is in the rules repo, the plan says that plainly
and stops short of forcing an engine workaround.

## 3. Gather context, not the whole codebase

Use the Explore agent (read-only) with a focused brief. What you need:

- Every symbol, file, config key, CLI flag, or error code the issue names, and
  where each is defined and used.
- The tests nearest that code: which file, which fixture pattern, how they
  build paths (see the Windows note in step 7).
- Helpers that already do part of the job. A fix that adds a second
  path-normaliser or a second "find the workspace root" is the wrong fix.
- How the same concern is handled elsewhere in the workspace, because the
  existing pattern is usually the one to follow.

Stop gathering when you can name the function that has the bug and the test
that should have caught it. Reading everything is slower and does not make the
plan better.

## 4. Reproduce before you plan

An explanation that was not reproduced is a hypothesis, and the plan should
label it as one.

- Engine: find or write the smallest failing unit test in the crate that owns
  the behaviour (`cargo test -p cwtools_<crate> <substring>`). If the issue
  names real mod files, run the CLI against them:
  `cargo run -p cwtools_cli -- validate --game hoi4 --directory ../Millennium-Dawn --rules ../cwtools-hoi4-config/Config`
  and compare with what the reporter saw.
- Extension: `extension/test/workspaces/stellaris` is the sample workspace the
  host tests run against; the node layer is `npm run test:node`.
- Tooling: `pytest tests/scripts -k <name>`.

If it does not reproduce, say so, state the hypothesis, and say what evidence
would confirm it. Do not paper over it by picking a fix that "should" help.

## 5. State the goal in one sentence

Write what the reporter actually needs, which is sometimes narrower or wider
than what they literally asked for. Name anything you are deliberately leaving
out of scope so the reviewer can disagree with the boundary rather than
discover it later.

## 6. Write the plan

Use this template, in this order:

```markdown
## Issue #<n>: <title>

**Goal:** one sentence.
**Root cause:** what is wrong and where (`file:line`). Prefix with "Hypothesis:" if not reproduced.
**Approach:** what changes and why this over the alternatives, in 2–3 lines.
**Changes:** `path` → what changes there (one line each).
**Tests:** which existing test file gets a case; what the failing assertion is.
**Verification:** fmt/clippy/test scope; whether the diagnostics guards run and why.
**Risks / open questions:** anything the reviewer should weigh in on.
**Alternatives rejected:** the other one or two ways, and why not.
```

## 7. The bar the approach has to clear

These are the habits of a careful Rust engineer on this particular repo. Each
one has a reason; the reason is what lets you recognise the cases it does not
cover.

- **Fix the cause, with the smallest diff that does.** A patch that suppresses
  the symptom at the call site leaves the bug for the next caller. A patch that
  refactors the module on the way through hides the fix in the noise.
- **Reuse before you add.** If step 3 found a helper that does most of the job,
  extend it or call it. Two near-identical helpers drift apart.
- **No new dependency without a reason that survives `cargo deny` and
  `cargo machete`.** Both run in CI; an unused or license-incompatible crate
  fails the build later, not now.
- **Don't widen `pub` for one caller.** Most crates are internal to the
  workspace. `pub(crate)` or moving the caller is usually the right shape.
- **Borrow over clone, `?` over `unwrap`, and no `#[allow]` to get to green.**
  If clippy or a type error is in the way, fix the cause — even when it predates
  the change.
- **Serialized types bump their cache version.** `FORMAT_VERSION` /
  `ERRORS_FORMAT_VERSION` in `engine/crates/cache/src/io.rs`, `CACHE_VERSION` in
  `engine/crates/index/src/vanilla_cache.rs` and
  `engine/crates/cache/src/workspace.rs`. The bump turns a stale cache into a
  miss instead of a load error.
- **Tests run on Windows CI.** No bare `/foo` absolute paths reaching
  `Url::from_file_path` or `Path::is_absolute`; use the `abs()` helper pattern
  from `engine/crates/cli/src/report.rs`. Don't assert an exact
  `Url::to_file_path` or `canonicalize` result without checking it holds on
  both platforms (`engine/crates/lsp/src/access.rs` shows the `#[cfg(unix)]`
  gate with a comment).
- **If diagnostics are meant to move, the guard re-bless is part of the
  plan**, with the codes that move and why, so the commit message can say it.
  A re-bless with no explanation reads as a regression someone papered over.

## 8. Approval gate

Present the plan, then ask with `AskUserQuestion`: **Implement** / **Adjust the
plan** / **Stop here**. With `--plan-only`, present the plan and stop.

## 9. Branch

If `git branch --show-current` prints `main`, create
`fix/<n>-<two-to-four-word-slug>` from the issue title (the convention already
in use: `fix/476-loc-text-per-file`). On any other branch, stay where you are
and say so in the hand-off — the user chose that branch.

## 10. Implement

Failing test first, then the fix, then watch it pass. Keep to the plan; when
the code turns out different from what the plan assumed, say what changed and
why rather than quietly widening the scope. Match the surrounding code's
naming, comment density, and idiom.

## 11. Verify

Run what the change touches, and report the real output.

Engine, from `engine/`:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p cwtools_<crate>      # while iterating
cargo test --workspace             # before calling it done
```

Then, from the repo root, for anything touching the parser, the rule engine, a
validator, or the ruleset types:

```bash
python3 scripts/guard.py md
python3 scripts/guard.py vanilla
```

The tests prove the code behaves; the guards prove the diagnostics did not
move. If a baseline is pinned to a corpus or rules revision that has moved on,
capture a before-baseline on a clean tree
(`CWTOOLS_BASELINE=<scratch>/before.csv python3 scripts/guard.py md --bless`)
and diff against that.

Extension: `npm run check` and `npm run test:node`; `npm test` for host-level
changes. Tooling: `ruff check scripts tests/scripts`, `black --check scripts
tests/scripts`, `pylint scripts tests/scripts`, `mypy scripts tests/scripts`,
`pytest`.

A check you did not run is not a check you can report. Say which ones ran and
which did not.

## 12. Changelog

Add a bullet under `### Unreleased` in `CHANGELOG.md`, in the matching
`#### Engine` / `#### Extension` / `#### Tooling` subsection, ending with
`(#<n>)`. Every substantive change updates the changelog; engine work included.

## 13. Commit and open the PR

Only once every check in step 11 passed. A red PR opened silently costs the
reviewer more than a report that says "clippy fails on X".

- `git add` only the files the plan touched plus `CHANGELOG.md`. If the tree
  had unrelated changes before this skill started, leave them unstaged and say
  so in the hand-off.
- Commit subject in the repo's `area: summary` form (`lsp:`, `parser:`,
  `validation:`, `client:`, `tests:`, `tooling:`), body naming the issue.
- `git push -u origin <branch>`, then `gh pr create --base main` with the same
  subject as the title and a body following `.github/PULL_REQUEST_TEMPLATE.md`:

  ```markdown
  ## What
  …
  ## Why
  …

  Fixes #<n>
  ```

## 14. Hand-off

Report: the goal, what changed (`file:line` references), which checks ran and
their result, which did not run and why, and the PR URL.
