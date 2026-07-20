# CWTools VS Code extension reference docs

This directory is intentional developer/user documentation. It is not copied into `release/`, so it is not shipped in the `.vsix`.

## Commands

Source: `release/package.json` `contributes.commands`.

- `genlocall` (`Generate missing loc for all files`)
  - Generates missing localisation keys across supported scripts.

- `reloadrulesconfig` (`Reload config rules`)
  - Reloads the rule configuration from disk.

- `cacheVanilla` (`Regenerate game vanilla cache file`)
  - Rebuilds vanilla cache data.

- `clearAllCaches` (`Clear all caches and reindex`)
  - Clears internal caches and triggers a full reindex.

- `reindexWorkspace` (`Re-index workspace`)
  - Re-parses and reindexes the current workspace.

- `cwtools.exportProfilingLog` (`Export profiling log`)
  - Exports the language server profiling buffer to a chosen file.

- `cwtools.showGraph` (`Show graph`)
  - Opens the dependency graph panel for the current file type.

- `cwtools.setGraphDepth` (`Set graph depth`)
  - Sets how many link hops from the current file to include in the graph.

- `cwtools.graphFromJson` (`Recreate graph from json`)
  - Loads graph data from a JSON export and renders it.

- `cwtools.saveGraphImage` (`Save graph as image`)
  - Saves the current graph as a PNG file.

- `cwtools.saveGraphJson` (`Save graph as json`)
  - Exports the current graph data as JSON.

## Settings

Source: `release/package.json` `contributes.configuration`.

- `cwtools.rules_folder` (string)
  - Default: empty
  - A folder containing custom rules to use.

- `cwtools.localisation.languages` (array)
  - Default: `["English"]`
  - Languages validated for localisation. Only these languages are checked.

- `cwtools.localisation.hoverShowAllLanguages` (boolean)
  - Default: `false`
  - Show hover localisation in every configured language instead of the first one.

- `cwtools.errors.ignore` (array)
  - Default: empty
  - Error/warning codes to suppress globally.

- `cwtools.errors.ignorefiles` (array)
  - Default: `["README.txt", "credits.txt", "credits_l_simp_chinese.txt", "reference.txt", "startup_info.txt"]`
  - File names excluded from workspace loading.

- `cwtools.ignore_patterns` (array)
  - Default: `["**/99_README**.txt"]`
  - Glob patterns for files to ignore.

- `cwtools.hover.debug` (boolean)
  - Default: `false`
  - Show raw rule classification in hover tooltips. For extension developers only.

- `cwtools.hover.scopeDisplay` (string)
  - Default: `"context"`
  - Enum: `context`, `resolved`
  - Controls hover scope display.

- `cwtools.cache.eu4` (string)
  - Path to vanilla EU4 install.

- `cwtools.cache.hoi4` (string)
  - Path to vanilla HoI4 install.

- `cwtools.cache.stellaris` (string)
  - Path to vanilla Stellaris install.

- `cwtools.cache.ck2` (string)
  - Path to vanilla CK2 install.

- `cwtools.cache.imperator` (string)
  - Path to vanilla Imperator: Rome install.

- `cwtools.cache.vic2` (string)
  - Path to vanilla Victoria II install.

- `cwtools.cache.ck3` (string)
  - Path to vanilla CK3 install.

- `cwtools.cache.vic3` (string)
  - Path to vanilla Victoria III install.

- `cwtools.cache.eu5` (string)
  - Path to vanilla EU5 install.

- `cwtools.graph.zoomSensitivity` (number)
  - Default: `1`
  - Control factor for scroll-wheel zoom in the graph.

- `cwtools.trace.server` (string)
  - Default: `"off"`
  - Enum: `off`, `messages`, `verbose`
  - Traces communication between VS Code and the language server.

- `cwtools.profiling` (boolean)
  - Default: `false`
  - Enables server profiling and keeps a buffer for export.

- `cwtools.backgroundReindex.intervalMinutes` (number)
  - Default: `30`
  - Background re-index interval in minutes. `0` disables.

## Graph view

The graph panel shows file/entity references as nodes and references as edges. Primary nodes are shown as the core set for the current file context; references are followed to build neighboring nodes.

Open the graph with `cwtools.showGraph`.

- Double-click a node in the graph to jump to its referenced file and position.
- Use `cwtools.setGraphDepth` to adjust hop distance and reopen the graph.
- Export the rendered graph from the panel with:
  - `cwtools.saveGraphImage`
  - `cwtools.saveGraphJson`
- Restore a graph from a previous export with `cwtools.graphFromJson`.

Graph actions above are in the command palette. Image/JSON export actions are tied to the graph view context.
