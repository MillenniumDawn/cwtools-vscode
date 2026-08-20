# CWTools Error Codes

Each diagnostic the validator emits carries a `CWxxx` code. The codes mirror the F# cwtools catalog with a few intentional renumberings (see [Reconciliations](#reconciliations) below). They are emitted from `crates/validation` and `crates/localization` (the loc-file and `[command]` checks), plus the CW600 block from `crates/rules`, and surfaced through the LSP server as editor diagnostics or printed to stdout by the CLI.

## Severity levels

| Severity | LSP equivalent | Typical use |
|---|---|---|
| Error | `error` | Definite defect -- game will likely not load or behave correctly |
| Warning | `warning` | Probable problem, or a thing that's usually wrong |
| Information | `information` | Style / performance hint; not a defect |
| Hint | `hint` | Low-priority suggestion |

## How to read an entry

**Status** values used below:

- **Emitted** -- the check runs and produces this code today.
- **Emitted (reconciled from F# CWxxx)** -- emitted today under the F# ID after an intentional renumber; the old Rust-invented ID is retired.
- **Defined, emission pending (subsystem)** -- the const exists but the check is not wired in; see [Pending subsystems](#currently-not-emitted-pending-subsystems).
- **Defined, not wired** -- the const exists but nothing emits it yet. Either it's superseded by a more specific code, or a generic check would need a complete registry to stay false-positive-safe (the project never trades correctness for coverage).
- **Emitted (escape hatch `CWTOOLS_...`)** -- runs by default; the env var disables it.
- **Emitted (needs a base-game index)** -- runs only when `--vanilla` or `--vanilla-cache` gave the run one. Every check with this status answers against the union of the mod's definitions and the base game's; with only the mod indexed it cannot tell a genuinely missing definition from one the base game supplies, so it reports nothing rather than flagging every vanilla reference. `cwtools validate` says so on stderr when it happens ("no base-game data loaded, so CW113, CW222, CW500 report nothing"), and carries the same line as a `::notice::` in the `github` report and a `toolConfigurationNotifications` entry in the `sarif` one. The codes are CW113, CW222, CW500 and, for Stellaris, CW227, CW229 and CW250.

Every row carries an anchor of its lowercased code, so `#cw113` lands on CW113
and keeps working when the headings around it change. The editor's "open
documentation" link on a diagnostic and the SARIF `helpUri` both point here that
way.

63 of F#'s 71 codes (after the experimental/dead cleanup) have a Rust
definition. Codes with no emission site in either engine were removed from
both; see [Removed](#removed-experimental--dead-deleted-from-both-engines).

The other 8 F# codes were superseded placeholders, dropped rather than kept
as unwired consts: CW002, CW249, CW998, CW999 had no path to emission;
CW101/102/103 are covered by the rules-engine structural codes CW262/263,
and CW241 by CW262-265.

## Suppressing a diagnostic

Three ways, all case-insensitive about the code:

- Inline, on the line it fires on: `# cwtools-ignore CW100`. The directive
  covers its own line and the lines directly above and below it, so the
  trailing form (`foo = bar # cwtools-ignore CW100`) and the standalone
  comment on either side both work. It names as many codes as you like
  (`# cwtools-ignore CW100 CW246`), and a second `#` ends the list, so a human
  note after it is not read as a code. Read from the raw source rather than the
  AST, so it works in loc files and in a file that failed to parse. A
  whole-file diagnostic (one with no line, like CW254) cannot be suppressed
  this way.
- Per run: `--ignore-code CWxxx` on the CLI (repeatable), or `ignore-codes` in
  `cwtools.toml`. `--only-code` is the inverse.
- Per workspace: the editor's `cwtools.errors.ignore` setting, which the "Ignore
  CWxxx in this workspace" code action writes for you.

The two escape hatches that turn off a whole family, `CWTOOLS_NO_SCOPE_CHECKS`
and `CWTOOLS_NO_VAR_CHECKS`, are described under
[Wired, runs by default](#wired-runs-by-default-with-an-escape-hatch).

---

## CW001 -- Localisation parse error

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw001"></a>CW001 | Error | Localisation file parse error: {} | A line in a `.yml` loc file could not be parsed (no `:` separator). The parser recovers and continues; one diagnostic per bad line. Mirrors F# `validateLocalisationSyntax` / `YAMLLocalisationParser` `Failure` path. | Emitted |

---

## CW100-CW122 -- Core: loc, variables, triggers/effects, scope, misc

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw100"></a>CW100 | Warning | Localisation key {} is not defined for {} | A referenced localisation key has no entry for the named language. Also covers the `## required` keys a type declares, both the name-derived form (`name = "$_desc"`) and the explicit-field form, where the key is the value of a child field (`title = title`). An instance that omits the explicit field is not flagged; the missing field is the rules' cardinality complaint. | Emitted |
| <a id="cw104"></a>CW104 | Error | {} trigger used in incorrect scope. In {} but expected {} | A trigger is used in a scope it doesn't accept. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`). Scope tracking handles links/iterators/data-refs/root-scope; a DLC-scope long tail may still surface false positives |
| <a id="cw105"></a>CW105 | Error | {} effect used in incorrect scope. In {} but expected {} | An effect is used in the wrong scope. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |
| <a id="cw106"></a>CW106 | Error | {} scope command used in incorrect scope. In {} but expected {} | A scope command is used outside its valid scope. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |
| <a id="cw107"></a>CW107 | Information | Event is missing mean_time_to_happen, is_triggered_only, fire_only_once, or trigger={always=no}. Performance concern: event may fire every tick. | An event has no guard against running every tick. | Emitted (reconciled from F# CW107 / formerly Rust CW300) |
| <a id="cw108"></a>CW108 | Error | This research_leader is missing required "area" | A `research_leader` block omits the required `area` field. | Emitted (Stellaris only; `research_leader` nested in `common/technology/*.txt`) |
| <a id="cw109"></a>CW109 | Information | This research_leader uses area {} but the technology uses area {} | The area in `research_leader` disagrees with the enclosing technology's area. | Emitted (Stellaris only; args leader-then-tech, F# had them swapped) |
| <a id="cw110"></a>CW110 | Error | No category found for this technology | A technology definition has no category. | Emitted (Stellaris only; any `common/technology/*.txt` root block) |
| <a id="cw113"></a>CW113 | Error | File {} not found, this is case sensitive | A file path referenced in script doesn't exist. Every indexed file (mod and base game, live or cache-restored) is matched by exact on-disk case, so a case-mismatched reference is flagged for case-sensitive filesystems (Linux/Mac). On by default; set `case-sensitive-files = false` in `cwtools.toml` (or pass `--case-sensitive-files false`) for a Windows-authored mod that must tolerate case mismatches. | Emitted (needs a base-game index; FilepathField refs are checked against the mod+vanilla file index) |
| <a id="cw120"></a>CW120 | Information | Trigger {} can be made a pretrigger (see code action to fix) | A trigger that could be promoted to a pretrigger for performance. | Emitted (Stellaris only; per-scope set, event `trigger` and pop-job `possible` blocks) |
| <a id="cw121"></a>CW121 | Warning | This 'if' trigger contains no effects | An `if` block contains only a `limit` or nothing at all. | Emitted |
| <a id="cw122"></a>CW122 | Information | Localisation key {} should not be quoted when used inline, this can cause unexpected behaviour | A loc key is wrapped in quotes where it is used inline. | Emitted |

---

## CW220-CW282 -- Loc references, event targets, bool/syntax hints, rules engine, type system

### CW220-CW222 -- Event targets / event index

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw220"></a>CW220 | Error | {} or an event it calls require the event target(s) {} but they are not set by this event or by all possible events leading here | A required event target is never set on any path leading to this event. | Defined, emission pending (event-target dataflow + cross-file event index) |
| <a id="cw221"></a>CW221 | Warning | {} or an event it calls require the event target(s) {} but they may not always be set by this event or by all possible events leading here | A required event target is not set on all paths leading to this event. | Defined, emission pending (event-target dataflow + cross-file event index) |
| <a id="cw222"></a>CW222 | Warning | The event id {} is not defined | A reference to an event id (`<event>`) that has no definition. | Emitted (needs a base-game index; relabeled from CW500 for `<event>` type refs) |

### CW223 -- Boolean/syntax structural hints

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw223"></a>CW223 | Information | Do not use NOT with multiple children, replace this with either NOR or NAND to avoid ambiguity | `NOT` wraps more than one child, which is ambiguous. | Emitted |

### CW225-CW226 -- Localisation cross-references

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw225"></a>CW225 | Error | Localisation key "{}" references "{}" which doesn't exist in {} | A loc string's `$KEY$` reference points to a key that has no definition. | Emitted |
| <a id="cw226"></a>CW226 | Error | Localisation key "{}" uses command "{}" which doesn't exist | A loc string's `[Command()]` single-segment Jomini call names a command not found in the scope registry (with a loaded registry). A `?`-marked variable read (`[?ROOT.war_support\|1]`) is checked against the project's variable registry: the config's built-in `value[variable]` reads plus every name the mod sets, so only a name neither knows is reported. A chain without the `?` ends in a command or a scripted-localisation name and stays lenient, as does one reading through a variable (`[?some_var.SomeLoc]`) or through a segment the scope engine can't resolve. A chain with an empty segment is skipped whole, which is what a parenthesised Jomini expression (`[(Character?.GetName:'CAP_SCIENTIST')]`) parses to. Mirrors F# `validateJominiLocalisationCommandsBase` `LocNotFound`. | Emitted (see [where the loc command checks run](#where-the-loc-command-checks-run)) |

### CW227-CW233 -- Section/component/mesh/entity (Stellaris-specific)

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw227"></a>CW227 | Error | Section template {} can not be found | A ship design references a section template that doesn't exist. | Emitted (needs a base-game index; Stellaris only; walks `ship_design`/`global_ship_design`; `DEFAULT_COLONIZATION_SECTION`/`DEFAULT_CONSTRUCTION_SECTION` exempt) |
| <a id="cw228"></a>CW228 | Error | Section template {} does not have a slot {} | A section template is referenced with a slot name it doesn't define. | Defined, emission pending (vanilla data registries) |
| <a id="cw229"></a>CW229 | Error | Component template {} can not be found | A ship design references a component template that doesn't exist. | Emitted (needs a base-game index; Stellaris only; walks `ship_design`/`global_ship_design`) |
| <a id="cw230"></a>CW230 | Warning | Component and slot do not match, slot {} has size {} and component {} has size {} | The size of a component doesn't fit the slot it's placed in. | Defined, emission pending (vanilla data registries) |
| <a id="cw231"></a>CW231 | Warning | Technology {} is not used | A technology definition is never referenced anywhere. | Emitted (Stellaris only; same reference map as CW239, minus F#'s exemptions: `prereqfor_desc`, `modifier`, `feature_flags`, `weight = 0`, `weight_modifier` factor 0) |
| <a id="cw233"></a>CW233 | Error | Entity {} is not defined | A section or other asset references an entity that isn't defined. | Defined, emission pending (vanilla data registries / asset index) |

### CW234-CW238 -- Loc placeholders, zero-modifier, if/else order

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw234"></a>CW234 | Information | Localisation key {} is a placeholder for {} | A loc value is `REPLACE_ME` or similar placeholder text. | Emitted |
| <a id="cw235"></a>CW235 | Warning | Modifier {} has value 0. Modifiers are additive so likely doesn't do anything | A known modifier is set to `0`, which is a no-op for additive modifiers. Fires wherever the key is a confirmed modifier: with no matching rule at all, and inside a `modifier = { ... }` block, where the key matches through the `modifier` alias. A key that matches a rule field of the block's own is left alone even when it shares a modifier's name, so a legitimate `factor = 0` is not flagged. | Emitted |
| <a id="cw236"></a>CW236 | Warning | Nested if/else in effects was deprecated with 2.1 and will be removed in a future release | Stellaris: nested `if/else` in effects, deprecated since 2.1. | Emitted |
| <a id="cw237"></a>CW237 | Information | 2.1 changed nested if = { if else } behaviour in effects. Check this still works as expected | Stellaris: ambiguous nested `if = { if else }` after 2.1 behaviour change. | Emitted |
| <a id="cw238"></a>CW238 | Error | An else/else_if is missing a preceding if | An `else` or `else_if` block has no antecedent, either as a preceding sibling (`if = {…} else = {…}`, Stellaris 2.1+) or as the enclosing `if`/`else_if` it nests inside (HOI4 and pre-2.1 Stellaris). | Emitted (cross-game, both chain spellings; CW236/CW237 remain Stellaris-only) |

### CW239 -- Unused type

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw239"></a>CW239 | Warning | {} of type {} is not used anywhere, but is expected to be | A `should_be_referenced` type instance is never referenced in any other file. | Emitted (reconciled from Rust CW502) |

Both CW239 and CW231 answer a project-wide question, so they run once at the end
of a batch run instead of per file: the rule engine records every `<type>`
reference it resolves, the driver merges those, and each file's own definitions
are then checked against the merged set. Nothing happens unless the config marks
a type `should_be_used` (Stellaris also tracks `technology`, for CW231), so on a
config that marks none the whole pass is skipped.

Two consequences worth knowing. In the editor the answer comes from a store the
workspace scan seeds and every revalidation keeps current, so an open file's
CW239 updates as references are added and removed, but a closed file's only
refreshes on the next scan (the CW100 contract). And only the mod's own
definitions are checked, since a run validates the mod, not the base game it
sits on.

### CW240-CW248 -- Rules-engine dynamic codes

These are the core rules-engine codes. Severity and message text are computed per-rule (the rule's `## severity` option overrides the defaults here).

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw240"></a>CW240 | Error | {} | A value didn't match its rule's field type (int/float/enum/bool/date, etc.). | Emitted |
| <a id="cw242"></a>CW242 | Warning | {} | A field appears too few or too many times (cardinality violation). | Emitted |
| <a id="cw243"></a>CW243 | Error | Target "{}" has incorrect scope. Is {} but expect {} | A scope target resolves to a scope the field doesn't expect. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |
| <a id="cw244"></a>CW244 | Error | {} is not a target. Expected a target in scope(s) {} | A value is not a valid target in any of the expected scopes. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |
| <a id="cw245"></a>CW245 | Error | Error in target. Link {} was used in scope {} but expected {} | A scope link inside a target chain was used in the wrong scope. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |
| <a id="cw246"></a>CW246 | Warning | The variable {} has not been set | A referenced variable hasn't been assigned anywhere the engine can see. | Emitted (escape hatch `CWTOOLS_NO_VAR_CHECKS=1`) |
| <a id="cw247"></a>CW247 | Error | Trigger/Effect/Modifier {} used in wrong scope. In {} but expect {} | A trigger, effect, or modifier rule was used in the wrong scope. | Emitted |
| <a id="cw248"></a>CW248 | Error | Invalid scope command {} | A scope command is not valid here. | Emitted (escape hatch `CWTOOLS_NO_SCOPE_CHECKS=1`) |

### CW250-CW253, CW280-CW283 -- Game-specific and cleanup hints

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw250"></a>CW250 | Error | {} | A planet-killer component template lacks its support script. | Emitted (needs a base-game index; Stellaris only; needs a matching `on_destroy_planet_with_<key>` on_action and `can_destroy_planet_with_<key>` scripted trigger) |
| <a id="cw251"></a>CW251 | Warning | This {} is unnecessary | A boolean operator (`AND`/`OR`) is nested directly inside an identical operator. | Emitted |
| <a id="cw253"></a>CW253 | Information | Consider using "set_name" instead for consistency | `set_empire_name` or `set_planet_name` should be replaced with `set_name`. | Emitted |
| <a id="cw280"></a>CW280 | Information | {} = { always = ... } matches the default and can be removed | HOI4 cleanup hint: a field whose body is exactly `{ always = <bool> }` matching the field's default (e.g. `allowed_civil_war = { always = no }`) is a no-op and can be deleted. Rust-original (no F# equivalent); field/default table in `per_game::hoi4`. | Emitted |
| <a id="cw281"></a>CW281 | Warning | This 'limit' contains no triggers | A `limit = { }` block with no conditions. An empty limit matches everything, so it is almost always forgotten conditions or dead weight. Rust-original (no F# equivalent); emitted from `per_game::structural`. | Emitted |
| <a id="cw282"></a>CW282 | Information | This is the default value ({}) and can be omitted | A bool field explicitly set to the engine default declared by the rule's `## default_bool` directive, so the line is redundant. Rust-original (no F# equivalent); emitted from `rule_core::children`. | Emitted |
| <a id="cw283"></a>CW283 | Error | Localisation key "{}" calls scripted GUI callback "{}" which does not exist | A HOI4 `[!name]` localisation call names no direct callback key under an indexed scripted GUI's `effects` or `triggers` container. Rust-only (no F# equivalent). | Emitted when the workspace or vanilla callback registry is populated |

### CW254-CW268 -- Localisation file headers and content

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw254"></a>CW254 | Error | Localisation files must be UTF-8 BOM, this file is not | A `.yml` loc file is not encoded as UTF-8 with BOM. | Emitted |
| <a id="cw255"></a>CW255 | Error | Localisation file name should contain (and ideally end with) "l_language.yml" | A loc file name contains no recognisable `l_xxx` language tag. | Emitted |
| <a id="cw256"></a>CW256 | Error | Localisation file should start with "l_language:" on the first line (or a comment) | A loc file's first content line is not a language header. | Emitted |
| <a id="cw257"></a>CW257 | Error | Localisation file's name has language {} doesn't match the header language {} | The language in the file name and the `l_xxx:` header disagree. | Emitted |
| <a id="cw258"></a>CW258 | Information | Localisation file name should end with "l_language.yml" | Language tag is present but not at the end of the file name. F# defines this but leaves emission commented out as "only convention"; cwtools-rs matches that -- const defined, never fired. | Retired / not emitted |
| <a id="cw259"></a>CW259 | Error | This localisation string refers to itself | A loc key's value includes a `$KEY$` reference back to the same key. | Emitted |
| <a id="cw260"></a>CW260 | Error | Loc command {} used in wrong scope. In {} but expected {} | A loc command is used in a data scope that doesn't support it. | Emitted (see [where the loc command checks run](#where-the-loc-command-checks-run)) |
| <a id="cw261"></a>CW261 | Error | Key {} of type {} is defined multiple times | The project defines the same instance id of a `unique` type more than once. Project-wide: a duplicate in another file counts, and every definition site is flagged, since any one of them is the candidate for deletion. Base-game definitions are excluded, so a mod redefining one reads as an override. | Emitted (reconciled from Rust CW501) |
| <a id="cw262"></a>CW262 | Error | {} | An unexpected `key = { ... }` node where the rule doesn't allow one. Also fires on a bad key inside a [math expression](MATH_EXPRESSIONS.md). | Emitted |
| <a id="cw263"></a>CW263 | Error | {} | An unexpected `key = value` leaf where the rule doesn't allow one. Also fires on a mis-typed operator inside a [math expression](MATH_EXPRESSIONS.md). | Emitted |
| <a id="cw264"></a>CW264 | Warning | {} | An unexpected bare value where the rule doesn't allow one. | Emitted |
| <a id="cw265"></a>CW265 | Warning | {} | An unexpected `{ ... }` value clause where the rule doesn't allow one. | Emitted |
| <a id="cw266"></a>CW266 | Error | Localisation key {} uses command {} which does not exist in data type {}. | A loc command is not valid in the resolved data type for that scope. | Emitted (reconciled from Rust CW262; see [where the loc command checks run](#where-the-loc-command-checks-run)) |
| <a id="cw267"></a>CW267 | Error | Expected a {} value, got {} | An alias key/value didn't match the expected alias category. | Emitted |
| <a id="cw268"></a>CW268 | Warning | Localisation key {} doesn't start and end with double quotes | A loc value is missing its enclosing double-quote delimiters. | Emitted |

### CW269-CW277 -- Optimisation, precision, custom errors, inline scripts, invalid chars, key validation

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw269"></a>CW269 | Hint | Optimise by merging this with {} by using {} | Two lists could be merged for a minor script optimisation. | Defined, emission pending (vanilla data registries) |
| <a id="cw270"></a>CW270 | Warning | Value too small, only 3 decimal places are supported in this context | A numeric value has more decimal places than the engine supports here. | Emitted (32-bit `variable_field` with >3 decimal places) |
| <a id="cw271"></a>CW271 | Warning | Expected an integer | A field that requires an integer received a float or non-numeric value. | Emitted (`int_variable_field` given a fractional value) |
| <a id="cw272"></a>CW272 | Error | {} | A custom message attached to a rule via `## error_if_only_match = ...`, raised when that overload is the only one a key matches. `## severity` on the same rule overrides Error. | Emitted |
| <a id="cw273"></a>CW273 | Warning | Modifier type {} is not defined but is used | A modifier's type reference points to a modifier-type that isn't defined. | Defined, emission pending (modifier-type registry) |
| <a id="cw274"></a>CW274 | Error | {} | An `inline_script` call whose body could not be pulled in: the named script doesn't exist, the call names none, its value isn't a `{ ... }` block, or the chain calls itself or nests more than 5 levels deep. A body that does resolve is validated against the rules and scope in force at the call site; its own diagnostics keep their own codes and are reported on the call site, each message naming the script line it came from. | Emitted (needs the mod's `common/inline_scripts`, which the CLI loads; the LSP does not load them yet, and accepts a call unexpanded) |
| <a id="cw275"></a>CW275 | Warning | Localisation value for {} contains unexpected characters, and may not render correctly | A loc value contains characters outside the expected set for that game. | Emitted |
| <a id="cw276"></a>CW276 | Warning | Localisation key {} contains invalid characters (spaces or special characters are not allowed) | A loc key contains a space or character not valid in a loc key (only alphanumeric, `_`, `.`, `-` are allowed). Rust-only (no F# equivalent). | Emitted |
| <a id="cw277"></a>CW277 | Warning | Validation stopped after reaching the alias branch limit | A file's recursive alias overloads exceeded the validator's per-file branch budget. Other diagnostics from that file, and project-wide unused-definition diagnostics from the run, may be incomplete. Rust-only (no F# equivalent). | Emitted |

---

## CW500 -- Type diagnostics (Rust-only)

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw500"></a>CW500 | Error | Type '{}' not found | A type name referenced in rules or script has no definition. No F# equivalent. | Emitted (needs a base-game index: the check only runs once the type index is complete) |

CW501 (duplicate type) and CW502 (unused type) were Rust-invented IDs that have been retired in favour of their F# equivalents CW261 and CW239 respectively.

---

## CW600-CW603 -- Rules config (Rust-only)

Problems in the `.cwt` ruleset itself rather than in the script it checks. Emitted from `crates/rules` as the config loads and reported against the `.cwt` file that carries them, so a broken ruleset surfaces in `cwtools rules`, in a `validate` report and in the editor's Problems panel instead of degrading every later check in silence. F# only ever printed these as text, so there is no ID to converge on.

| ID | Severity | Message | Meaning | Status |
|---|---|---|---|---|
| <a id="cw600"></a>CW600 | Error | Rules file could not be read: {} | A `.cwt` file or rules directory the loader could not read: a missing or unreadable path, an unreadable directory entry, or a file over the scan budget. The offending path is the diagnostic's file, so a directory-targeted one lands on the folder. | Emitted |
| <a id="cw601"></a>CW601 | Error | Rule references undefined {} `{}` | A rule names a type, enum or single_alias that no `.cwt` file defines. Resolved after every file is merged, so a cross-file definition counts; alias categories are deliberately out of scope. | Emitted |
| <a id="cw602"></a>CW602 | Error | {} | A `single_alias_right[...]` the post-processor refused to expand: a reference cycle, a chain past the depth limit, or the node budget. Reported on the `single_alias` definition it names, which is where the fix goes. | Emitted |
| <a id="cw603"></a>CW603 | Warning | {} | A `##` directive whose value the loader can't parse (a malformed `cardinality` bound, an unrecognised `severity`), so the option silently falls back to its default. One rule under-checks; the ruleset still loads. | Emitted |

---

## Reconciliations

These are intentional ID renumberings documented in `error_codes.rs`. All converge Rust-invented codes onto their F# equivalents so downstream baselines key off a single consistent number.

- **CW501 -> CW261** (`DuplicateTypeDef`): cwtools-rs originally emitted duplicate-type errors as CW501; converged to F#'s CW261. CW501 is retired.
- **CW502 -> CW239** (`UnusedType`): cwtools-rs reserved CW502 for unused-type errors; converged to F#'s CW239. CW502 is retired.
- **CW300 -> CW107** (`EventEveryTick`): cwtools-rs originally emitted this as CW300 at Warning severity; F# emits it as CW107 at Information (performance hint, not a defect). Converged to CW107.
- **CW262 -> CW266** (loc-command-not-in-data-type): cwtools-rs originally used CW262 for `LocCommandNotInDataType`; CW262 belongs to F#'s `ConfigRulesUnexpectedPropertyNode`. Renumbered to CW266.
- **CW400 -> CW247** (`ConfigRulesRuleWrongScope`): the `## scope` rule-requirement check originally emitted the Rust-invented CW400; converged to F#'s CW247. CW400 is retired.
- **CW201-CW205 -> CW262-CW265 / CW240 / CW242** (rules-engine structural codes): cwtools-rs invented CW200-CW205 for rules-engine mismatches. These were replaced with the exact F# IDs: CW262/263/264/265 for the four node-kind-specific "unexpected property" variants, CW240 for unexpected value, and CW242 for cardinality violations.

---

## Currently not emitted (pending subsystems)

63 of F#'s 71 codes have a Rust definition (see the note near the top of this
doc for the other 8). The codes below are defined but not yet emitted: each
needs a subsystem Rust doesn't have yet, and wiring it without that machinery
would false-positive on valid game config (which the project forbids). They
are kept so only the emission site remains to be built. None are HOI4/Millennium
Dawn blockers; most are Stellaris/other-game checks that need that game's
corpus to validate.

| Subsystem needed | Codes blocked |
|---|---|
| Event-target dataflow + cross-file event index | CW220, CW221 |
| Per-template field data (slots/sizes) + asset index | CW228, CW230, CW233 |
| List-merge optimisation hint | CW269 |
| Modifier-type registry | CW273 |

### Where the loc command checks run

**CW226, CW260, CW266** and **CW283** judge a `[command]` chain in a loc value against
the game's scopes and links, so they need a ruleset. Two passes run them, and
they see different things:

- `cwtools validate` and the language server run them at each reference site,
  seeded with the scope of the field using the key. That is the stricter answer:
  a chain valid in one scope and wrong in another is caught where it is wrong.
- `cwtools loc --game <game> --rules <path>` runs them over every entry in the
  scan, starting from an unknown scope. There are no game files to name a
  reference site, so only what is wrong in *every* scope is reported.

CW226's scripted-variable registry follows the same split. The two passes that
walk the game files collect one (the config's built-in `value[variable]` reads
plus every name the mod sets), so they check what a `?`-marked read names.
`cwtools loc` reads the `.yml` files and the ruleset only, and never walks the
game files the mod's own variable names come from; a registry holding just the
built-ins would call every mod-set variable undefined, so it is withheld and
every multi-segment chain stays lenient there. `validate` withholds it the same
way while its variable index is still empty, which is what the language server
sees before its first scan finishes.

A chain without the `?` ends in a terminal command or a scripted-localisation
name, and CW226 and CW266 need a second registry for that half: the
`defined_text` names the project defines. They are read from the folder
(`common/scripted_localisation`, or `common/scripted_loc` on the Jomini games)
rather than through the ruleset's `scripted_loc` type, because the HOI4 config
declares that type at Stellaris's path and so never matches a HOI4 file (#348).
The same withholding rule applies: no names, no judgment. Only the two passes
that walk the game files have them, so `cwtools loc` reports neither code on a
command tail and keeps CW260 alone, and `validate` is lenient until its scan
finishes. A base-game install contributes its names too, through the vanilla
cache.

HOI4's `[!name]` form bypasses normal scope-command handling and checks `name`
against a separate scripted-GUI callback registry. That registry contains only
direct keys under each GUI's `effects` and `triggers` containers in
`common/scripted_guis`, including base-game names restored from the vanilla
cache. A missing registry stays lenient; once populated, a missing callback
reports CW283 rather than CW226 or CW266.

`cwtools loc` without both settings loads no registry and reports none of these
four; the file-level loc checks (CW225, CW254-CW259, CW268, CW275, CW276) do
not need one and always run.

### Wired, runs by default (with an escape hatch)

The scope family is config-driven and ON by default; set `CWTOOLS_NO_SCOPE_CHECKS=1`
to disable: **CW104, CW105, CW106, CW243, CW244, CW245, CW247, CW248, CW260**.

The "variable has not been set" check (**CW246**) is also ON by default; set
`CWTOOLS_NO_VAR_CHECKS=1` to disable. It only applies to the `variable` value set
(and arrays, which the engine stores the same way), accepts the built-ins declared
in `variables.cwt`, and skips names that cannot be resolved statically (`@`-vars,
inline math, `$ARG$`-built names, and `prefix:` reads). Millennium Dawn reports 60
hits and Kaiserreich 1, all genuine unset variables.

### Rust-only extensions (no F# equivalent)

- **CW283** — a `[!name]` localisation call whose scripted-GUI callback does not exist.
- **CW500** — an `<type>` reference that resolves to no known instance (the event-specific case is F#'s CW222).
- **CW600-CW603** — problems in the `.cwt` ruleset itself, which F# only ever printed as text.

CW301 (pre-trigger at event root) was a Rust-invented ID that duplicated F#'s
CW120 on the same leaf; it has been retired in favour of CW120.

### Removed (experimental / dead, deleted from both engines)

CW111, CW112, CW114, CW115, CW116, CW117, CW118, CW119, CW224, CW232 had no
emission site in F# (several were flagged "Experimental, please report errors").
They were deleted from the F# source and the Rust catalog. The retired
renumbering placeholders CW252 and CW400 were also removed. If button/sprite,
static-modifier, modifier, mesh, or undefined-script-variable validation is wanted
later, it is a fresh feature with a fresh code, not parity work. (CW117's
"variable never defined" intent is covered by the live CW246.)
