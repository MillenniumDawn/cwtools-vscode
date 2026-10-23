# Workspace diagnostics baseline

A diagnostics baseline suppresses diagnostics already known in a selected mod, so new diagnostics remain visible while existing ones do not. Set `cwtools.diagnostics.baselineFile` to a workspace-relative file path to enable it; leave it empty to disable suppression. The setting is scoped to the workspace folder served by CWTools.

Run **CWTools: Write Diagnostics Baseline** to create or refresh the configured baseline. If no file is configured, the command writes `.cwtools-baseline` in the workspace. The command accepts an optional workspace-relative target path. Writing runs with cancellable progress; the setting is changed only after the server confirms a successful write. The language server checks containment before writing.

Each non-comment line contains a 16-character hexadecimal hash. Current content hashes and legacy line-number hashes are accepted. The writer sorts and deduplicates current hashes, including diagnostics already suppressed by the active baseline. Ignored codes and inline-ignore directives still apply.

Captures include unsaved open buffers and closed files even when workspace-wide diagnostics are off. A busy, cancelled, incomplete, or stale capture leaves the previous baseline unchanged. Targets must stay inside the selected workspace, cannot escape through symlinks, and must be missing or already contain a valid baseline.

CLI `--output-hashes` and `--ignore-hashes` use the same hashes. Source text is decoded like the engine, and a leading file BOM does not affect the hash. Regenerate older baselines for diagnostics in CP1252 files or on a UTF-8 BOM's first line. Identical diagnostics on identical lines in one file share a hash.

Changes to the configured baseline file reload suppression. Clearing the setting or removing hashes restores script, localisation, and rule diagnostics. Missing or invalid baselines do not suppress diagnostics; invalid or unreadable files log a warning. Suppressed diagnostics do not supply quick fixes.

The status bar reports the number of currently suppressed diagnostics when the server is ready. A zero count removes the suppression suffix. Restarting or stopping the server resets that count until it reports status again.
