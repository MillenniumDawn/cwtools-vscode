# Workspace diagnostics baseline

A diagnostics baseline suppresses diagnostics already known in a selected mod, so new diagnostics remain visible while existing ones do not. Set `cwtools.diagnostics.baselineFile` to a workspace-relative file path to enable it; leave it empty to disable suppression. The setting is scoped to the workspace folder served by CWTools.

Run **CWTools: Write Diagnostics Baseline** to create or refresh the configured baseline. If no file is configured, the command writes `.cwtools-baseline` in the workspace. The command accepts an optional workspace-relative target path. Writing runs with cancellable progress; the setting is changed only after the server confirms a successful write. The language server validates containment and is authoritative for whether the target is safe to write.

The status bar reports the number of currently suppressed diagnostics when the server is ready. A zero count removes the suppression suffix. Restarting or stopping the server resets that count until it reports status again.
