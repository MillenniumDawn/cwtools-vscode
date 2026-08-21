# [CWTools MD Edition](https://github.com/MillenniumDawn/cwtools-vscode)

**Paradox Language Features for Visual Studio Code**

## Origin

This is a fork of [cwtools/cwtools-vscode](https://github.com/cwtools/cwtools-vscode). The original extension wrapped an F# language server from [cwtools/cwtools](https://github.com/cwtools/cwtools). This fork ships the Rust engine from this repository's [`engine/`](https://github.com/MillenniumDawn/cwtools-vscode/tree/main/engine) workspace.

## Disclaimer

This extension is still in preview, it may not work, it may stop working at any time.
**Make backups of your mod files.**

## Supported games

* Stellaris
* Hearts of Iron IV
* Europa Universalis IV
* Europa Universalis V - in progress, help needed
* Imperator: Rome - outdated, help needed
* Crusader Kings II - partial
* Crusader Kings III - in progress, help needed
* Victoria 2 - in progress, help needed
* Victoria 3 - in progress, help needed

## Features

* Immediate highlighting of syntax errors
* Autocomplete while you type, providing descriptions when available
* Tooltips on hover showing:
  * Related localisation
  * Documentation for that element
  * Scope context at that position
* A wide range of validators for common, interface, and events, checking
  * That required localisation keys are defined
  * Existence of effects/triggers/modifiers
  * Scope context for used effects/triggers/modifiers
  * Usage of scripted effects/triggers
  * Correct entries for weights/AI_chance/etc
  * That event\_targets are saved before they're used
  * That referenced sprites and graphics files exist
  * and a number of other specific validators

## Usage

1. Install this extension
2. Open your mod folder directly, which should be within a folder containing the game name:

* `C:\Users\name\Documents\Paradox Interactive\Stellaris\mod\your_mod`

1. Set the `cwtools.cache.<game>` setting (e.g. `cwtools.cache.hoi4`) to the game's install folder
2. Edit files and watch syntax errors show up when you make mistakes
3. Wait up to a minute for the extension to scan your mod and find errors

### Multiple mods - workspace

If you have multiple mods that need to be loaded at once, use VS Code's [multi-root workspace](https://code.visualstudio.com/docs/editing/workspaces/workspaces#_untitled-multiroot-workspaces) feature.

1. Open your first mod
2. Use "File", "Add folder to workspace" to add your next mod
3. cwtools should reload including both mods and vanilla in context using correct mod load order

If you want to browse vanilla files, you can use the "CWTOOLS LOADED FILES" section in the Explorer tab.

### Completion

![Completion](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/completion.gif)

### Tooltips

![Tooltips](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/tooltips.gif)

### Scope tooltips

![Scope tooltips](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/scopetooltip.gif)

### Scope errors

![Scope ](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/scopeerror.gif)

### Localisation error

![Localisation error](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/localisationerror.gif)

### Go to definition

![Go to definition](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/gotodef.gif)

### Find all references

![Find all references](https://raw.githubusercontent.com/MillenniumDawn/cwtools-vscode/refs/heads/main/docs/extension/media/findallrefs.png)

### Background reindex

The extension periodically re-scans the whole workspace in the background, so files changed outside the editor and definitions moved between files don't go stale until you reload the window. The rescan is idle-gated: it waits until you stop typing before running. To force one immediately, run **cwtools: Re-index workspace** from the command palette. `cwtools.backgroundReindex.intervalMinutes` controls the interval (default 30; 0 disables the automatic pass) and `cwtools.backgroundReindex.idleSeconds` how long you have to be idle before a pass may start (default 15).

### Graph view

The graph view draws entity references as a node graph you can pan, zoom, and click through to the source. **cwtools: Show graph** loads graph data from the bundled server, **Set graph depth** controls how many reference hops are included, and **cwtools: Recreate graph from json** opens a saved export. The live commands appear only when the running server advertises graph support.

### Reporting a slow session

If the editor feels slow, turn on `cwtools.profiling`, reload the window, reproduce the slowdown, then run **cwtools: Export profiling log** and attach the file to an issue. It records per-phase timing and memory for the language server. Leave it off the rest of the time.

## Theming

The extension ships its own [TextMate grammar](https://github.com/cwtools/paradox-syntax/tree/master/syntaxes) for the supported games, so syntax highlighting works out of the box. No second extension to install.

The grammars are vendored from [cwtools/paradox-syntax](https://github.com/cwtools/paradox-syntax) (see [`scripts/sync_paradox_syntax.py`](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/scripts/sync_paradox_syntax.py) to refresh them); the `.cwt` rules grammar is owned here. Themes live under [`extension/package/themes/`](https://github.com/MillenniumDawn/cwtools-vscode/tree/main/extension/package/themes). Every theme paints the full scope set from both grammars (game scripts and `.cwt` rule files) plus a generic baseline, so coloring is consistent whatever file you're in. Pick one with the Color Theme picker:

* [**Paradox - Classic**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Classic.tmLanguage.json). Dark
  scheme modeled on VS Code's default Dark+ token colours, applied to the
  full scope set (paradox, cwt, loc). Pairs the editor colours you already
  know with the per-scope slots the rest of the themes use.
* [**Paradox - Syntax**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Syntax.tmLanguage.json). Dark
  scheme modeled on VS Code's Dark+.
* [**Paradox - Kate**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Kate.tmLanguage.json) and
  [**Paradox - Kate Light**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Kate-Light.tmLanguage.json).
  Inspired by KDE's
  [Kate / Breeze](https://invent.kde.org/frameworks/syntax-highlighting)
  palette.
* [**Paradox - Nord**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Nord.tmLanguage.json).
  [Nord](https://www.nordtheme.com) palette.
* [**Paradox - High Contrast**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-HighContrast.tmLanguage.json)
  and [**Paradox - High Contrast Light**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-HighContrast-Light.tmLanguage.json).
  Accessibility variants, saturated tokens on black/white.
* [**Paradox - Dimmed**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-Dimmed.tmLanguage.json) and
  [**Paradox - Quiet Light**](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/extension/package/themes/Paradox-QuietLight.tmLanguage.json).
  Re-homed from the upstream
  [tboby.paradox-syntax](https://marketplace.visualstudio.com/items?itemName=tboby.paradox-syntax)
  extension.

Highlighting runs off a single merged `paradox` grammar, with each game's keywords folded into it, so there are no separate per-game grammar files to keep in sync.

Themes also carry a semantic token layer from the language server, which paints the tokens a static grammar can't disambiguate. See [Adding a theme](https://github.com/MillenniumDawn/cwtools-vscode/blob/main/docs/extension/themes.md) for how to build a theme and what that layer covers.

## Credits

The TextMate grammars under [`extension/package/syntaxes/`](https://github.com/MillenniumDawn/cwtools-vscode/tree/main/extension/package/syntaxes) are vendored from the [cwtools/paradox-syntax](https://github.com/cwtools/paradox-syntax) extension (Copyright (c) 2018 Thomas Boby, MIT). Original authors and contributors:

* **Thomas Boby** (publisher
  [`tboby`](https://marketplace.visualstudio.com/publishers/tboby)) and
  **Dayshine**, with contributions from **Gratak** and others. See the upstream
  [contributors list](https://github.com/cwtools/paradox-syntax/graphs/contributors)
  and [commit history](https://github.com/cwtools/paradox-syntax/commits/master).

The bundled themes draw on the following:

* **Paradox - Dimmed** is by **Gratak** (Adam Przybylski), re-homed from the
  upstream
  [Paradox-Dimmed source](https://github.com/cwtools/paradox-syntax/blob/master/themes/Paradox-Dimmed.tmLanguage.json).
* **Paradox - Quiet Light** is by **UristMcDorf**, re-homed from the upstream
  [Paradox-QuietLight source](https://github.com/cwtools/paradox-syntax/blob/master/themes/Paradox-QuietLight.tmLanguage.json).
* **Paradox - Kate** and **Paradox - Kate Light** take their palette from KDE's
  [Kate / Breeze syntax highlighting](https://invent.kde.org/frameworks/syntax-highlighting)
  (Kate Editor, [KDE community](https://kde.org)).
* **Paradox - Nord** uses the [Nord](https://www.nordtheme.com) palette (Arctic
  Ice Studio, MIT). See the
  [Nord repo](https://github.com/nordtheme/visual-studio-code).
* **Paradox - High Contrast** and **Paradox - High Contrast Light** follow
  [VS Code's built-in Default High Contrast](https://code.visualstudio.com/docs/getstarted/themes#_high-contrast-theme)
  theme (Microsoft).

`cwtools-md-edition` itself is a fork of
[cwtools/cwtools-vscode](https://github.com/cwtools/cwtools-vscode) maintained
by the [Millennium Dawn mod team](https://github.com/MillenniumDawn). The Rust
language server it drives began as
[MillenniumDawn/cwtools](https://github.com/MillenniumDawn/cwtools), itself a
fork of [cwtools/cwtools](https://github.com/cwtools/cwtools), and now lives in
this repo under [`engine/`](https://github.com/MillenniumDawn/cwtools-vscode/tree/main/engine).

## Links

* [vic2-config](https://github.com/cwtools/cwtools-vic2-config)
* [vic3-config](https://github.com/cwtools/cwtools-vic3-config)
* [ck2-config](https://github.com/cwtools/cwtools-ck2-config)
* [eu4-config](https://github.com/cwtools/cwtools-eu4-config)
* [hoi4-config](https://github.com/cwtools/cwtools-hoi4-config)
* [stellaris-config](https://github.com/cwtools/cwtools-stellaris-config)
* [ir-config](https://github.com/cwtools/cwtools-ir-config)
* [ck3-config](https://github.com/cwtools/cwtools-ck3-config)
