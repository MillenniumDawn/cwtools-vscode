# Adding a theme

The bundled themes live under `release/themes/`. Each is a VS Code color theme
manifest that paints the full scope set from the paradox grammar and the `.cwt`
rules grammar, plus a semantic token layer contributed by the language server.

## Anatomy of a theme

Every theme has two highlighting layers:

* `tokenColors` — TextMate scope colors, painted from the grammar. This covers
  the bulk of the language: keys, effects, triggers, modifiers, comments,
  numbers, strings, booleans, and so on.
* `semanticTokenColors` — colors for the tokens the language server classifies
  from parsed structure, where the grammar alone cannot tell them apart. This is
  the layer that resolves the ambiguity the issue in CHANGELOG 2.5.0 documents
  (for example a CK2 define key vs a scope keyword, or `capital` as a keyword vs
  a field).

A theme opts into semantic highlighting with `"semanticHighlighting": true`, and
the extension defaults `editor.semanticHighlighting.enabled` to on for the
`paradox` and `cwt` languages via `configurationDefaults` in `release/package.json`.

## Semantic token types

The server advertises this legend (see `semantic.rs` in the engine). The themes
paint a curated subset; the rest fall back to their TextMate scope colors.

| type | meaning | colored in themes? |
|---|---|---|
| `comment` | comment | no — scope color |
| `property` | a leaf/block key | no — scope color |
| `operator` | `=`, `>=`, `!=`, … | no — scope color |
| `number` | number literal | no — scope color |
| `string` | unclassified scalar, LocRef, FileRef | no — scope color |
| `keyword` | `yes` / `no` | no — scope color |
| `type` | TypeRef value, or a type-declaring key | yes |
| `type.declaration` | `type` with the declaration modifier (the key names a type instance) | yes |
| `enumMember` | EnumRef value | yes |
| `variable` | script variable read | yes |
| `namespace` | scope name | yes |
| `function` | key resolved through an alias category | yes |

The colored types get deliberate, distinct colors in each theme so the server's
structural disambiguation is visible. Dark, light, and high-contrast variants
carry their own values.

## Adding a theme

1. Copy an existing manifest (for example `Paradox-Nord.tmLanguage.json`) to a
   new file under `release/themes/`.
2. Update `name`, `type` (`dark`/`light`/`high-contrast`), and the `colors` and
   `tokenColors` to your palette.
3. Add or adjust `semanticTokenColors`. The block is small and self-contained;
   copy the shape from an existing theme of the same family and change the hex
   values to match your palette. Leave `"semanticHighlighting": true` in place.
4. Register the theme in `release/package.json` under the `themes` array
   (`label`, `uiTheme`, `path`).
5. Update this README's Theming section to list it.

Semantic highlighting must be enabled for your theme's colors to appear. The
extension already sets `editor.semanticHighlighting.enabled` for `paradox` and
`cwt`, so a bundled theme only needs `"semanticHighlighting": true`. A user theme
that is not bundled can enable it the same way or via
`editor.semanticTokenColorCustomizations` in their settings.
