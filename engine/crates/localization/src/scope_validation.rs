//! Scope-aware localisation command validation.
//!
//! Validates chains like `[THIS.Owner.GetName]` by folding through the game's
//! `ScopeContext`.  Emits `LocCommandWrongScope` or `LocCommandChainEndsInScope`
//! when a chain is invalid.  Unknown commands are accepted leniently so missing
//! entries don't produce false positives.

use crate::commands::LocEntry;
use crate::loc_string::JominiCommand;
use cwtools_game::constants::Game as EngineGame;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId, ScopeResult};
use cwtools_game::scope_registry::ScopeRegistry;
use rustc_hash::FxHashSet;
use std::sync::Arc;

// ── Public types ──────────────────────────────────────────────────────────────

/// A diagnostic produced by `validate_loc_commands`.
#[derive(Debug, Clone, PartialEq)]
pub enum LocCommandDiagnostic {
    /// A scope-change link was used from an incompatible scope.
    ///
    /// Mirrors F# `LocContextResult.WrongScope`.
    WrongScope {
        /// The command segment that triggered the error.
        command: String,
        /// Numeric ID of the current scope at the point of failure.
        current_scope: u32,
        /// Numeric IDs of the scopes the command is valid in.
        expected_scopes: Vec<u32>,
    },
    /// The chain ended without reaching a terminal getter command.
    ///
    /// Mirrors F#'s "chain ends in scope rather than terminal command" check.
    ChainEndsInScope {
        /// Full command string that ended without a getter.
        command: String,
    },
    /// The command was not found in the scope registry at all.
    ///
    /// Mirrors F# `LocNotFound` / CW226 `InvalidLocCommand`.
    /// Only emitted when a scope registry is present (config-driven mode);
    /// without one the validator remains fully lenient to avoid false positives.
    NotFound {
        /// The unrecognised command segment.
        command: String,
    },
    /// A `[!name]` call names no indexed scripted-GUI callback.
    ScriptedGuiNotFound {
        /// The callback name without the `!` marker.
        callback: String,
    },
}

/// Whether a chain segment names a variable the project defines.
///
/// The variable index sits above this crate, so the caller supplies the probe.
/// It is handed the segment with its `|format` suffix already stripped; the
/// caller's own normalization (`@` concatenation, `?`/`^` selectors, case)
/// applies on top. `Sync` because both the per-file validation pass and the
/// standalone loc lint fan out over rayon holding a `&LocScopeData`.
// The variable, scripted-loc, and scripted-GUI registries share one lookup shape.
pub type ScriptedVariables<'a> = &'a (dyn Fn(&str) -> bool + Sync);

/// Per-game static data needed for loc-command validation.
///
/// The caller constructs this from their game configuration and passes it to
/// `validate_loc_commands`.  Using a struct keeps the function signature
/// stable while the data grows.
pub struct LocScopeData<'a> {
    /// Game variant (controls which scope links are loaded).
    pub game: Option<EngineGame>,
    /// Terminal getter commands accepted for this game. Lowercased.
    ///
    /// If this is empty every unknown command is accepted (fully lenient).
    /// If non-empty, any unknown final segment not in this list will produce
    /// a `ChainEndsInScope` diagnostic.
    pub terminal_commands: FxHashSet<String>,
    /// Whether `?variable` syntax is accepted (HOI4 / Stellaris).
    pub question_mark_variable: bool,
    /// Whether `parameter:xxx` references are accepted.
    pub parameter_variables: bool,
    /// Config-driven scope/link registry. When set, the loc scope engine uses it
    /// (shared with the validation path) instead of the hardcoded per-game table.
    pub registry: Option<Arc<ScopeRegistry>>,
    /// Scripted-variable registry, consulted before CW226 fires on an unknown
    /// final segment. `None` keeps every multi-segment chain lenient, which is
    /// what a run with no variable index gets.
    pub scripted_variables: Option<ScriptedVariables<'a>>,
    /// Scripted-localisation registry, consulted alongside terminal commands
    /// before CW226 fires. Mirrors `scripted_variables` but for the final
    /// tail of a command chain (`AST_GetNavyName` etc). `None` means the run
    /// has no scripted-localisation data at all, which leaves every unknown
    /// tail lenient: nothing can tell a scripted localisation from a typo, and
    /// calling them all typos is what #348 was.
    pub scripted_locs: Option<ScriptedVariables<'a>>,
    /// Scripted-GUI callback registry for `[!name]` calls. `None` leaves those
    /// calls lenient until workspace or vanilla callback data is available.
    pub scripted_guis: Option<ScriptedVariables<'a>>,
}

impl Default for LocScopeData<'_> {
    fn default() -> Self {
        Self {
            game: None,
            terminal_commands: FxHashSet::default(),
            question_mark_variable: true,
            parameter_variables: true,
            registry: None,
            scripted_variables: None,
            scripted_locs: None,
            scripted_guis: None,
        }
    }
}

/// Build the loc scope context: from the config registry when provided (shared
/// with the validation path), else from the game's hardcoded table.
fn build_loc_ctx(
    data: &LocScopeData<'_>,
    engine_game: EngineGame,
    initial: ScopeId,
) -> ScopeContext {
    match &data.registry {
        Some(reg) => ScopeContext::from_registry(reg.clone(), initial),
        None => ScopeContext::new(engine_game, initial),
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Validate all `[command]` and `[JominiCommand chain]` blocks in a loc entry.
///
/// * `entry`       — the parsed loc entry whose commands/jomini_commands to check.
/// * `initial_scope` — the scope context active where this loc string appears.
///   Pass `ScopeId(0)` (SCOPE_ANY) when the context is unknown.
/// * `data`        — per-game static settings.
///
/// Returns a (possibly empty) list of diagnostics.
pub fn validate_loc_commands(
    entry: &LocEntry,
    initial_scope: ScopeId,
    data: &LocScopeData<'_>,
) -> Vec<LocCommandDiagnostic> {
    // Nothing to validate: skip building the terminal set / engine mapping and
    // return the empty (non-allocating) Vec for the common no-command entry.
    if entry.commands.is_empty() && entry.jomini_commands.is_empty() {
        return Vec::new();
    }

    // `build_loc_ctx` ignores `engine_game` whenever a config-driven registry
    // is present (it takes the `from_registry` branch), so computing it — and
    // the no-mapping warn inside it — is dead work per leaf in that case.
    let engine_game = if data.registry.is_some() {
        EngineGame::Hoi4 // unused: build_loc_ctx takes the registry branch
    } else {
        match game_to_engine(data.game) {
            Some(g) => g,
            // No registry and no hardcoded scope table for this game (CK2,
            // VIC2, Custom, or unset): there is nothing correct to validate
            // against, so skip the scope check rather than judge it by HOI4's.
            None => return Vec::new(),
        }
    };
    let mut diags = Vec::new();

    // `terminal_commands` is already lowercased (from RuleSet). Use directly.
    let terminal_set = &data.terminal_commands;

    // Validate legacy [command] strings (single-segment, dot-split internally)
    for cmd in &entry.commands {
        validate_command_string(
            cmd,
            initial_scope,
            engine_game,
            data,
            terminal_set,
            &mut diags,
        );
    }

    // Validate Jomini command chains — each inner Vec is one bracket's chain.
    for chain in &entry.jomini_commands {
        validate_jomini_chain(
            chain,
            initial_scope,
            engine_game,
            data,
            terminal_set,
            &mut diags,
        );
    }

    diags
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Pick the engine `Game` whose scope table drives loc-command validation.
///
/// The seven games with a scope table map to themselves; everything else
/// (`None`, CK2, VIC2, Custom) has no table, so the caller skips the check
/// for that entry rather than judge it against a different game's scopes.
fn game_to_engine(game: Option<EngineGame>) -> Option<EngineGame> {
    static NO_MAPPING_WARNED: std::sync::Once = std::sync::Once::new();
    match game {
        Some(
            g @ (EngineGame::Hoi4
            | EngineGame::Stellaris
            | EngineGame::Eu4
            | EngineGame::Ck3
            | EngineGame::Ir
            | EngineGame::Vic3
            | EngineGame::Eu5),
        ) => Some(g),
        other => {
            // Without a registry this can run once per loc-referencing leaf;
            // warn only the first time per run rather than flooding the log.
            NO_MAPPING_WARNED.call_once(|| {
                tracing::warn!(
                    "localization game {:?} has no engine mapping; skipping loc command scope validation",
                    other
                );
            });
            None
        }
    }
}

/// Returns true if `lower` (an already-lowercased command/segment) is a special
/// prefix that bypasses scope checks.
///
/// Mirrors F# handling of `event_target:`, `parameter:`, `?`. The caller
/// lowercases once and shares the result with `is_terminal_command`.
fn is_bypass_prefix(lower: &str, data: &LocScopeData<'_>) -> bool {
    lower.starts_with("event_target:")
        || lower.starts_with("scope:")
        || (data.parameter_variables && lower.starts_with("parameter:"))
        || (data.question_mark_variable && lower.starts_with('?'))
}

/// Per-segment pre-check shared by both chain validators: decide whether the
/// segment bypasses scope checks, terminates the chain, or needs a scope change.
///
/// `seg_lower` is the already-lowercased segment; `is_last` marks the final
/// segment of the chain.
enum SegmentPre {
    /// `event_target:` / `parameter:` / `?…` etc. — push `SCOPE_ANY` and continue.
    Bypass,
    /// Terminal getter (`Get…` or in the terminal list) as the final segment —
    /// the chain is accepted, stop walking.
    TerminalStop,
    /// Ordinary segment: attempt `ctx.change_scope` and classify the result.
    ScopeChange { looks_terminal: bool },
}

fn classify_segment(
    seg_lower: &str,
    is_last: bool,
    data: &LocScopeData<'_>,
    terminal_set: &FxHashSet<String>,
) -> SegmentPre {
    if is_bypass_prefix(seg_lower, data) {
        return SegmentPre::Bypass;
    }
    let looks_terminal = is_terminal_command(seg_lower, terminal_set);
    if is_last && looks_terminal {
        return SegmentPre::TerminalStop;
    }
    SegmentPre::ScopeChange { looks_terminal }
}

/// Neutral classification of a `ScopeResult` for the scope-change arm, shared by
/// both chain validators. The two callers differ only in how they treat
/// `Unknown` on the final segment and how they track lenient intermediates, so
/// those decisions stay in the callers; everything else is shared here.
enum ScopeOutcome {
    /// Scope advanced normally (`NewScope` / `VarFound`); keep walking.
    Advanced,
    /// `AnyScope` — advanced leniently (callers may note an "any" intermediate).
    AnyScope,
    /// `ValueFound` at the end of the chain — accept and stop.
    ValueEnd,
    /// `ValueFound` mid-chain — lenient stop-continue (no scope progress, but the
    /// chain is accepted as-is; F# would error, we don't).
    ValueMid,
    /// A scope-change link used from an incompatible scope; carries the data to
    /// build the `WrongScope` diagnostic (the caller formats the `command`).
    Wrong {
        command: String,
        current: ScopeId,
        expected: Vec<ScopeId>,
    },
    /// `NotFound` / `VarNotFound` — unknown segment; caller decides final vs.
    /// intermediate policy.
    Unknown,
}

fn classify_scope_result(result: ScopeResult, is_last: bool) -> ScopeOutcome {
    match result {
        ScopeResult::NewScope { .. } | ScopeResult::VarFound => ScopeOutcome::Advanced,
        ScopeResult::AnyScope => ScopeOutcome::AnyScope,
        ScopeResult::ValueFound if is_last => ScopeOutcome::ValueEnd,
        ScopeResult::ValueFound => ScopeOutcome::ValueMid,
        ScopeResult::WrongScope {
            command,
            current,
            expected,
        } => ScopeOutcome::Wrong {
            command,
            current,
            expected,
        },
        ScopeResult::NotFound | ScopeResult::VarNotFound(_) => ScopeOutcome::Unknown,
    }
}

/// Validate a legacy dot-delimited command string, e.g. `THIS.Owner.GetName`.
fn validate_command_string(
    cmd: &str,
    initial_scope: ScopeId,
    engine_game: EngineGame,
    data: &LocScopeData<'_>,
    terminal_set: &FxHashSet<String>,
    diags: &mut Vec<LocCommandDiagnostic>,
) {
    if is_bypass_prefix(&cmd.to_ascii_lowercase(), data) {
        return;
    }

    let segments: Vec<&str> = cmd.split('.').collect();
    let last_idx = segments.len().saturating_sub(1);

    let mut ctx = build_loc_ctx(data, engine_game, initial_scope);

    for (i, seg) in segments.iter().enumerate() {
        let is_last = i == last_idx;
        if is_last && validate_scripted_gui_callback(seg, data, diags) {
            return;
        }

        // Lowercase once per segment; shared by the bypass and terminal checks.
        let seg_lower = seg.to_ascii_lowercase();

        let looks_terminal = match classify_segment(&seg_lower, is_last, data, terminal_set) {
            SegmentPre::Bypass => {
                ctx.push_scope(SCOPE_ANY);
                continue;
            }
            // Terminal command — no scope check needed; accept.
            SegmentPre::TerminalStop => break,
            SegmentPre::ScopeChange { looks_terminal } => looks_terminal,
        };

        match classify_scope_result(ctx.change_scope(seg), is_last) {
            // Scope changed (incl. AnyScope) or a value-only trigger: this path
            // does not track lenient intermediates, so all of these just continue.
            ScopeOutcome::Advanced
            | ScopeOutcome::AnyScope
            | ScopeOutcome::ValueEnd
            | ScopeOutcome::ValueMid => {}
            ScopeOutcome::Wrong {
                command,
                current,
                expected,
            } => {
                diags.push(LocCommandDiagnostic::WrongScope {
                    command: format!("{} (in {})", command, cmd),
                    current_scope: current.0,
                    expected_scopes: expected.iter().map(|s| s.0).collect(),
                });
                // Short-circuit: further segments are meaningless
                return;
            }
            ScopeOutcome::Unknown => {
                // Unknown command.  If it's the final segment and we have no
                // terminal-commands list, accept it (lenient); if we have a
                // non-empty list and it didn't match, warn.
                // NOTE: CW226 (NotFound) is NOT emitted here because this path
                // handles legacy `[command]` strings (single-segment, no dots).
                // F# only fires CW226 from validateJominiLocalisationCommandsBase,
                // not from validateLocalisationCommandsBase. Legacy commands like
                // `[var_name|fmt]` or `[2%%Y]` are valid HOI4 loc syntax and are
                // not scope links, so we remain lenient here.
                //
                // A bare `[SomeScriptedLoc]` reaches this arm rather than the
                // Jomini one (no dot to split on), so it is judged against the
                // same scripted-localisation registry (#348).
                if is_last
                    && !data.terminal_commands.is_empty()
                    && !looks_terminal
                    && data.scripted_locs.is_some()
                    && !is_scripted_loc(seg, data)
                {
                    diags.push(LocCommandDiagnostic::ChainEndsInScope {
                        command: cmd.to_string(),
                    });
                } else if !is_last {
                    // Unknown intermediate — push ANY and continue leniently.
                    ctx.push_scope(SCOPE_ANY);
                }
            }
        }
    }
}

/// Validate a Jomini command chain (one `[...]` bracket's segments).
///
/// Scope is threaded through the segments left-to-right, mirroring
/// `validate_command_string`. This is the single scope-threading implementation
/// for Jomini chains; the old per-segment path is replaced.
fn validate_jomini_chain(
    chain: &[JominiCommand],
    initial_scope: ScopeId,
    engine_game: EngineGame,
    data: &LocScopeData<'_>,
    terminal_set: &FxHashSet<String>,
    diags: &mut Vec<LocCommandDiagnostic>,
) {
    if chain.is_empty() || chain.iter().any(|cmd| cmd.key.is_empty()) {
        return;
    }
    let last_idx = chain.len() - 1;
    let mut ctx = build_loc_ctx(data, engine_game, initial_scope);
    // A `?` marks the bracket as a variable read (`[?ROOT.war_support|1]`), so the
    // final segment is a variable name the scripted-variable registry answers for.
    // A chain reading through a variable (`[?GER_crisis_id.GERGetCrisisType]`)
    // has an opaque intermediate value and stays lenient. Other multi-segment
    // chains (e.g. `ROOT.GetName`, `ROOT.AST_GetNavyName`) end in a terminal
    // command or a scripted-localisation name and are validated against the
    // rules/config registries. Unknown intermediates (country-tag scopes like
    // PAL) poison either kind, which is tracked below.
    let marker = chain[0].key.strip_prefix('?');
    let has_q_mark = data.question_mark_variable && marker.is_some();
    let reads_through_variable = has_q_mark && marker.is_some_and(|m| reads_a_variable(m, data));
    let lacks_variable_registry = has_q_mark && data.scripted_variables.is_none();
    // A chain without the `?` ends in a terminal command or a scripted-localisation
    // name. With no scripted-localisation registry nothing can tell one from a typo,
    // so the tail stays lenient rather than reporting every one of them (#348).
    let lacks_loc_registry = !has_q_mark && data.scripted_locs.is_none();
    let mut had_lenient_intermediate =
        (reads_through_variable || lacks_variable_registry) && chain.len() > 1;

    for (i, cmd) in chain.iter().enumerate() {
        let seg = &cmd.key;
        let is_last = i == last_idx;
        if is_last && validate_scripted_gui_callback(seg, data, diags) {
            return;
        }

        // Lowercase once per segment; shared by the bypass and terminal checks.
        let seg_lower = seg.to_ascii_lowercase();

        let looks_terminal = match classify_segment(&seg_lower, is_last, data, terminal_set) {
            SegmentPre::Bypass => {
                ctx.push_scope(SCOPE_ANY);
                // The `?` marker is not an unresolved segment, only the scope it
                // is attached to; `event_target:` and friends really are opaque.
                if !is_last && !seg_lower.starts_with('?') {
                    had_lenient_intermediate = true;
                }
                continue;
            }
            // terminal — accepted without scope check
            SegmentPre::TerminalStop => return,
            SegmentPre::ScopeChange { looks_terminal } => looks_terminal,
        };

        match classify_scope_result(ctx.change_scope(seg), is_last) {
            ScopeOutcome::AnyScope => {
                if !is_last {
                    had_lenient_intermediate = true;
                }
            }
            ScopeOutcome::Advanced | ScopeOutcome::ValueMid => {}
            ScopeOutcome::ValueEnd => return,
            ScopeOutcome::Wrong {
                command,
                current,
                expected,
            } => {
                diags.push(LocCommandDiagnostic::WrongScope {
                    command,
                    current_scope: current.0,
                    expected_scopes: expected.iter().map(|s| s.0).collect(),
                });
                return; // short-circuit
            }
            ScopeOutcome::Unknown => {
                if is_last
                    && data.registry.is_some()
                    && !looks_terminal
                    && !had_lenient_intermediate
                    && !lacks_loc_registry
                    && !reads_a_variable(seg, data)
                    && !is_scripted_loc(seg, data)
                {
                    // Registry present, chain resolved cleanly up to this point,
                    // final segment is neither a known command, a defined variable,
                    // nor a scripted-localisation: CW226 (mirrors F# `LocNotFound`).
                    diags.push(LocCommandDiagnostic::NotFound {
                        command: seg.to_string(),
                    });
                } else if !is_last {
                    ctx.push_scope(SCOPE_ANY); // unknown intermediate: lenient
                    had_lenient_intermediate = true;
                }
            }
        }
    }
}

fn validate_scripted_gui_callback(
    segment: &str,
    data: &LocScopeData<'_>,
    diags: &mut Vec<LocCommandDiagnostic>,
) -> bool {
    let Some(callback) = segment.strip_prefix('!') else {
        return false;
    };
    let callback = callback.split('|').next().unwrap_or(callback).trim();
    if let Some(lookup) = data.scripted_guis
        && !lookup(callback)
    {
        diags.push(LocCommandDiagnostic::ScriptedGuiNotFound {
            callback: callback.to_string(),
        });
    }
    true
}

fn is_known_name(segment: &str, lookup: Option<ScriptedVariables<'_>>) -> bool {
    let Some(lookup) = lookup else {
        return false;
    };
    let name = segment.split('|').next().unwrap_or(segment).trim();
    if name.is_empty() || name.contains(':') || name.contains('$') {
        return true;
    }
    name.parse::<f64>().is_ok() || lookup(name)
}

/// Whether `segment` reads a variable the scripted-variable registry can vouch
/// for, so a chain ending on it is legitimate rather than a typo.
///
/// The segment carries the `|format` suffix loc syntax allows (`my_var|R0`),
/// which the registry never holds; strip it before asking. Three forms are taken
/// on trust because their written text is not a name to look up: a bare number
/// the read formats (`[?0.3|-=%1]`), a `holder:name` read through a scope the
/// engine resolves at runtime, and a `$ARG$`-concatenated name.
fn reads_a_variable(segment: &str, data: &LocScopeData<'_>) -> bool {
    is_known_name(segment, data.scripted_variables)
}

fn is_scripted_loc(segment: &str, data: &LocScopeData<'_>) -> bool {
    is_known_name(segment, data.scripted_locs)
}

/// Check if a command segment is (or looks like) a terminal getter.
///
/// Terminal commands end the chain and return a string/value — they don't
/// produce a new scope.
///
/// This covers the common Paradox naming convention (`GetName`, `GetDesc`,
/// `GetRuler`…) plus the per-game list provided in `LocScopeData`.
/// `lower` is the already-lowercased segment; `terminal_set` is the lowercased
/// terminal-command set from `LocScopeData`.
fn is_terminal_command(lower: &str, terminal_set: &FxHashSet<String>) -> bool {
    // Convention: terminal getters start with "Get" (case-insensitive)
    lower.starts_with("get") || terminal_set.contains(lower)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{LocEntry, Position};
    use crate::loc_string::JominiCommand;

    fn make_entry_with_commands(commands: Vec<String>) -> LocEntry {
        LocEntry {
            key: "test_key".into(),
            value: None,
            desc: "test".into(),
            position: Position::new(Arc::from("test.yml"), 1, 1),
            desc_column: 10,
            error_range: None,
            refs: Vec::new(),
            commands,
            jomini_commands: Vec::new(),
        }
    }

    fn make_entry_with_jomini(jomini: Vec<Vec<JominiCommand>>) -> LocEntry {
        LocEntry {
            key: "test_key".into(),
            value: None,
            desc: "test".into(),
            position: Position::new(Arc::from("test.yml"), 1, 1),
            desc_column: 10,
            error_range: None,
            refs: Vec::new(),
            commands: Vec::new(),
            jomini_commands: jomini,
        }
    }

    fn hoi4_data() -> LocScopeData<'static> {
        // HOI4 is config-driven: supply a minimal registry (country/state +
        // owner/controller links) so the scope chains resolve in tests.
        use cwtools_game::scope_engine::{ScopeId, ScopeLink};
        let mut reg = ScopeRegistry::default();
        for (name, id) in [("country", 100u32), ("state", 101u32)] {
            reg.by_name.insert(name.to_string(), ScopeId(id));
            reg.by_id.insert(
                ScopeId(id),
                cwtools_game::scope_registry::ScopeDefOwned {
                    name: name.to_string(),
                    aliases: vec![name.to_string()],
                    subscope_of: vec![],
                },
            );
        }
        for name in ["owner", "controller"] {
            reg.links.insert(
                name.to_string(),
                ScopeLink {
                    valid_scopes: vec![ScopeId(101)], // state only
                    target: Some(ScopeId(100)),       // -> country
                    ignore_keys: vec![],
                },
            );
        }
        LocScopeData {
            game: Some(EngineGame::Hoi4),
            terminal_commands: ["GetName", "GetNameDef", "GetAdjective", "GetLeader"]
                .into_iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            question_mark_variable: true,
            parameter_variables: true,
            registry: Some(Arc::new(reg)),
            scripted_variables: None,
            // Present but empty: the project defines no scripted localisation, so
            // an unknown tail really is a typo. `None` would mean "no data", which
            // is a different answer (see the lenient tests below).
            scripted_locs: Some(&|_: &str| false),
            scripted_guis: Some(&|_: &str| false),
        }
    }

    /// `hoi4_data` plus a registry that knows one variable, `war_support`.
    fn hoi4_data_with_variables() -> LocScopeData<'static> {
        LocScopeData {
            scripted_variables: Some(&|name: &str| name.eq_ignore_ascii_case("war_support")),
            ..hoi4_data()
        }
    }

    // ── Valid chain: State → owner (Country) → GetName ────────────────────────

    #[test]
    fn valid_chain_state_owner_getname() {
        // Starting in HOI4 State (101): owner → Country (100) → GetName (terminal)
        let entry = make_entry_with_commands(vec!["owner.GetName".into()]);
        let data = hoi4_data();

        // Start in State scope (HOI4 State = 101)
        let diags = validate_loc_commands(&entry, ScopeId(101), &data);
        assert!(
            diags.is_empty(),
            "owner.GetName from State scope should be valid, got: {:?}",
            diags
        );
    }

    // ── Invalid chain: Country → controller (only valid from State) ───────────

    #[test]
    fn invalid_chain_country_controller_wrong_scope() {
        // Starting in HOI4 Country (100): `controller` is only valid from State (101)
        let entry = make_entry_with_commands(vec!["controller.GetName".into()]);
        let data = hoi4_data();

        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            !diags.is_empty(),
            "controller from Country scope should produce a WrongScope diagnostic"
        );
        assert!(
            matches!(diags[0], LocCommandDiagnostic::WrongScope { .. }),
            "expected WrongScope, got: {:?}",
            diags
        );
    }

    // ── Bypass: event_target: is always accepted ──────────────────────────────

    #[test]
    fn event_target_bypass() {
        let entry = make_entry_with_commands(vec!["event_target:my_target".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "event_target: should always be accepted");
    }

    // ── Bypass: parameter: is accepted ───────────────────────────────────────

    #[test]
    fn parameter_bypass() {
        let entry = make_entry_with_commands(vec!["parameter:my_param".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "parameter: should always be accepted");
    }

    // ── Bypass: ?variable is accepted ────────────────────────────────────────

    #[test]
    fn question_mark_variable_bypass() {
        let entry = make_entry_with_commands(vec!["?some_var".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "?variable should always be accepted");
    }

    // ── THIS/Root/PREV/FROM primary scopes ───────────────────────────────────

    #[test]
    fn primary_scope_this_getname() {
        let entry = make_entry_with_commands(vec!["THIS.GetName".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(101), &data);
        assert!(diags.is_empty(), "THIS.GetName should always be valid");
    }

    #[test]
    fn primary_scope_root_getname() {
        let entry = make_entry_with_commands(vec!["Root.GetName".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(101), &data);
        assert!(diags.is_empty(), "Root.GetName should always be valid");
    }

    // ── Jomini single-command GetName accepted ────────────────────────────────

    #[test]
    fn jomini_getname_accepted() {
        // A single bracket [GetName] — one chain with one segment
        let entry = make_entry_with_jomini(vec![vec![JominiCommand {
            key: "GetName".into(),
            params: Vec::new(),
        }]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "Jomini GetName should be accepted as terminal"
        );
    }

    // ── Jomini wrong-scope link produces diagnostic ───────────────────────────

    #[test]
    fn jomini_wrong_scope_controller_from_country() {
        // `controller` is only valid from State (101), not Country (100)
        let entry = make_entry_with_jomini(vec![vec![JominiCommand {
            key: "controller".into(),
            params: Vec::new(),
        }]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            !diags.is_empty(),
            "Jomini controller from Country should produce WrongScope"
        );
        assert!(
            matches!(diags[0], LocCommandDiagnostic::WrongScope { .. }),
            "expected WrongScope, got: {:?}",
            diags
        );
    }

    // ── Jomini dot-chain threads scope correctly ──────────────────────────────

    #[test]
    fn jomini_chain_state_owner_getname() {
        // [owner.GetName] from State scope: owner → Country → terminal
        let entry = make_entry_with_jomini(vec![vec![
            JominiCommand {
                key: "owner".into(),
                params: Vec::new(),
            },
            JominiCommand {
                key: "GetName".into(),
                params: Vec::new(),
            },
        ]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(101), &data);
        assert!(
            diags.is_empty(),
            "owner.GetName from State should be valid, got: {:?}",
            diags
        );
    }

    #[test]
    fn jomini_chain_wrong_scope_in_middle() {
        // [controller.GetName] from Country (100): controller is State-only
        let entry = make_entry_with_jomini(vec![vec![
            JominiCommand {
                key: "controller".into(),
                params: Vec::new(),
            },
            JominiCommand {
                key: "GetName".into(),
                params: Vec::new(),
            },
        ]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            !diags.is_empty(),
            "controller from Country should produce WrongScope, got: {:?}",
            diags
        );
    }

    // ── CW226: unknown final command when registry is present ─────────────────

    #[test]
    fn unknown_final_command_with_registry_emits_not_found() {
        // `totally_unknown` is not a scope link, not a getter (no "Get" prefix),
        // not in the terminal-commands list, and we have a registry.
        // Mirrors F# `LocNotFound` → CW226.
        let entry = make_entry_with_jomini(vec![vec![JominiCommand {
            key: "totally_unknown".into(),
            params: Vec::new(),
        }]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert_eq!(
            diags.len(),
            1,
            "expected one NotFound diagnostic: {:?}",
            diags
        );
        assert!(
            matches!(diags[0], LocCommandDiagnostic::NotFound { .. }),
            "expected NotFound, got: {:?}",
            diags
        );
    }

    #[test]
    fn unknown_final_command_without_registry_is_lenient() {
        // No registry → fully lenient, no CW226.
        let entry = make_entry_with_jomini(vec![vec![JominiCommand {
            key: "totally_unknown".into(),
            params: Vec::new(),
        }]]);
        let data = LocScopeData {
            game: Some(EngineGame::Hoi4),
            terminal_commands: FxHashSet::default(),
            question_mark_variable: true,
            parameter_variables: true,
            registry: None, // no registry
            scripted_variables: None,
            ..Default::default()
        };
        let diags = validate_loc_commands(&entry, ScopeId(0), &data);
        assert!(
            diags.is_empty(),
            "without registry, unknown command should be accepted: {:?}",
            diags
        );
    }

    // ── #339: no engine mapping and no registry means no table to check
    //    against, so the scope check is skipped rather than judged by HOI4's ──

    fn no_mapping_data(game: EngineGame) -> LocScopeData<'static> {
        LocScopeData {
            game: Some(game),
            terminal_commands: ["GetName"]
                .into_iter()
                .map(str::to_ascii_lowercase)
                .collect(),
            question_mark_variable: true,
            parameter_variables: true,
            registry: None,
            scripted_variables: None,
            scripted_locs: Some(&|_: &str| false),
            scripted_guis: Some(&|_: &str| false),
        }
    }

    #[test]
    fn ck2_without_registry_skips_loc_scope_validation() {
        // Would be `ChainEndsInScope` under the old HOI4-fallback behavior.
        let entry = make_entry_with_commands(vec!["totally_unknown_command".into()]);
        let data = no_mapping_data(EngineGame::Ck2);
        let diags = validate_loc_commands(&entry, ScopeId(0), &data);
        assert!(
            diags.is_empty(),
            "CK2 has no engine mapping and no registry; the scope check must be \
             skipped, not run against HOI4's table: {diags:?}"
        );
    }

    #[test]
    fn vic2_without_registry_skips_loc_scope_validation() {
        let entry = make_entry_with_commands(vec!["totally_unknown_command".into()]);
        let data = no_mapping_data(EngineGame::Vic2);
        let diags = validate_loc_commands(&entry, ScopeId(0), &data);
        assert!(
            diags.is_empty(),
            "VIC2 has no engine mapping and no registry; the scope check must be \
             skipped, not run against HOI4's table: {diags:?}"
        );
    }

    #[test]
    fn mapped_game_without_registry_is_still_checked() {
        // Contrast with the skip above: Stellaris DOES have a hardcoded scope
        // table, so without a registry it must still be validated, not skipped.
        let entry = make_entry_with_commands(vec!["totally_unknown_command".into()]);
        let data = no_mapping_data(EngineGame::Stellaris);
        let diags = validate_loc_commands(&entry, ScopeId(0), &data);
        assert_eq!(
            diags,
            vec![LocCommandDiagnostic::ChainEndsInScope {
                command: "totally_unknown_command".into(),
            }],
            "stellaris has a scope table and must still be checked: {diags:?}"
        );
    }

    #[test]
    fn registry_present_path_unaffected_by_game_mapping() {
        // `validate_loc_commands` never calls `game_to_engine` when a registry
        // is present, so a CK2 entry with a registry validates through the
        // registry exactly as HOI4 does — the #339 skip only ever applies to
        // the no-registry, no-mapping combination.
        let entry = make_entry_with_commands(vec!["controller.GetName".into()]);
        let data = LocScopeData {
            game: Some(EngineGame::Ck2),
            ..hoi4_data()
        };
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            matches!(diags[0], LocCommandDiagnostic::WrongScope { .. }),
            "a registry-backed CK2 entry must still be validated through the \
             registry: {diags:?}"
        );
    }

    // ── #350: scripted-GUI callback calls ─────────────────────────────────────

    #[test]
    fn bare_scripted_gui_callback_is_case_insensitive() {
        let entry = make_entry_with_commands(vec!["!TOPBAR_ICON_CLICK".into()]);
        let mut data = hoi4_data();
        data.scripted_guis = Some(&|name: &str| name.eq_ignore_ascii_case("topbar_icon_click"));
        assert!(validate_loc_commands(&entry, ScopeId(100), &data).is_empty());
    }

    #[test]
    fn unknown_bare_scripted_gui_callback_has_dedicated_diagnostic() {
        let entry = make_entry_with_commands(vec!["!missing_click".into()]);
        assert_eq!(
            validate_loc_commands(&entry, ScopeId(100), &hoi4_data()),
            vec![LocCommandDiagnostic::ScriptedGuiNotFound {
                callback: "missing_click".into(),
            }]
        );
    }

    #[test]
    fn unknown_dotted_scripted_gui_callback_has_dedicated_diagnostic() {
        let entry = chain(&["Root", "!missing_click"]);
        assert_eq!(
            validate_loc_commands(&entry, ScopeId(100), &hoi4_data()),
            vec![LocCommandDiagnostic::ScriptedGuiNotFound {
                callback: "missing_click".into(),
            }]
        );
    }

    #[test]
    fn dotted_scripted_gui_callback_still_checks_its_prefix_scope() {
        let entry = chain(&["controller", "!topbar_icon_click"]);
        let mut data = hoi4_data();
        data.scripted_guis = Some(&|name: &str| name == "topbar_icon_click");
        assert!(matches!(
            validate_loc_commands(&entry, ScopeId(100), &data).as_slice(),
            [LocCommandDiagnostic::WrongScope { .. }]
        ));
    }

    #[test]
    fn scripted_gui_callback_without_registry_is_lenient() {
        let entry = make_entry_with_commands(vec!["!missing_click".into()]);
        let data = LocScopeData {
            scripted_guis: None,
            ..hoi4_data()
        };
        assert!(validate_loc_commands(&entry, ScopeId(100), &data).is_empty());
    }

    // ── #348: no scripted-localisation registry means no judgment ─────────────

    /// The run has a scope registry but no scripted-localisation data (the
    /// standalone `loc` lint, and the editor before its first scan finishes).
    /// A tail could be a `defined_text` as easily as a typo, so neither the
    /// chain path nor the legacy single-segment path may call it one.
    fn hoi4_data_without_loc_registry() -> LocScopeData<'static> {
        LocScopeData {
            scripted_locs: None,
            ..hoi4_data()
        }
    }

    #[test]
    fn chain_without_scripted_loc_registry_stays_lenient() {
        let entry = chain(&["Root", "AST_GetNavyName"]);
        let data = hoi4_data_without_loc_registry();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "no scripted-loc registry must leave the tail unjudged: {diags:?}"
        );
    }

    #[test]
    fn bare_command_without_scripted_loc_registry_stays_lenient() {
        let entry = make_entry_with_commands(vec!["AST_GetNavyName".into()]);
        let data = hoi4_data_without_loc_registry();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "the legacy path must be lenient too: {diags:?}"
        );
    }

    /// A bare `[SomeScriptedLoc]` has no dot to split on, so it reaches
    /// `validate_command_string` rather than the chain walker. That path judged
    /// the tail against `terminal_commands` alone and reported CW266 for every
    /// scripted localisation (#348).
    #[test]
    fn bare_scripted_loc_command_is_accepted() {
        let entry = make_entry_with_commands(vec!["AST_GetNavyName".into()]);
        let mut data = hoi4_data();
        data.scripted_locs = Some(&|name: &str| name.eq_ignore_ascii_case("AST_GetNavyName"));
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "a bare scripted-localisation command must be accepted: {diags:?}"
        );
    }

    #[test]
    fn bare_unknown_command_still_ends_in_scope() {
        // The other half: with the registry present, a name it does not know is
        // still reported, so the check keeps catching real mistakes.
        let entry = make_entry_with_commands(vec!["Receiving Country".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert_eq!(
            diags,
            vec![LocCommandDiagnostic::ChainEndsInScope {
                command: "Receiving Country".into(),
            }],
        );
    }

    // ── CW226 on multi-segment chains, gated by the variable registry ─────────

    fn chain(segments: &[&str]) -> LocEntry {
        make_entry_with_jomini(vec![
            segments
                .iter()
                .map(|s| JominiCommand {
                    key: (*s).into(),
                    params: Vec::new(),
                })
                .collect(),
        ])
    }

    #[test]
    fn chain_reading_a_defined_variable_is_accepted() {
        // [?Root.war_support|1]: the registry knows the name, and the `|1` format
        // suffix is not part of it.
        let entry = chain(&["?Root", "war_support|1"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "a read of a defined variable should be accepted: {diags:?}"
        );
    }

    #[test]
    fn chain_reading_an_undefined_variable_emits_not_found() {
        // Same shape, misspelt: nothing in the registry answers for it.
        let entry = chain(&["?Root", "war_suport"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert_eq!(
            diags,
            vec![LocCommandDiagnostic::NotFound {
                command: "war_suport".into(),
            }],
        );
    }

    #[test]
    fn chain_reading_through_a_variable_stays_lenient() {
        // [?war_support.SomeScriptedLoc]: the marked segment is itself a variable,
        // so the tail is a command again and no registry can answer for it.
        let entry = chain(&["?war_support", "SomeScriptedLoc"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "a read through a variable should stay lenient: {diags:?}"
        );
    }

    #[test]
    fn command_chain_typo_is_flagged() {
        // No `?`: the final segment is checked against terminal commands and
        // scripted-localisations; a typo must flag CW226.
        let entry = chain(&["Root", "war_suport"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert_eq!(
            diags,
            vec![LocCommandDiagnostic::NotFound {
                command: "war_suport".into(),
            }],
        );
    }

    #[test]
    fn command_chain_with_terminal_is_accepted() {
        let entry = chain(&["Root", "GetName"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "a terminal command tail should be accepted: {diags:?}"
        );
    }

    #[test]
    fn command_chain_with_scripted_loc_is_accepted() {
        let entry = chain(&["Root", "AST_GetNavyName"]);
        let mut data = hoi4_data_with_variables();
        data.scripted_locs = Some(&|name: &str| name.eq_ignore_ascii_case("AST_GetNavyName"));
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "a scripted-localisation tail should be accepted: {diags:?}"
        );
    }

    #[test]
    fn chain_with_unknown_intermediate_stays_lenient() {
        // `PAL` is a country tag, not a scope link: the chain never resolved, so
        // its final segment is not judged.
        let entry = chain(&["?Root", "PAL", "war_suport"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "an unresolved intermediate should keep the chain lenient: {diags:?}"
        );
    }

    #[test]
    fn multi_segment_chain_without_variable_registry_stays_lenient() {
        // The pre-registry behaviour, still what a run with no variable index gets.
        let entry = chain(&["?Root", "war_suport"]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "without a variable registry every multi-segment chain is exempt: {diags:?}"
        );
    }

    #[test]
    fn get_prefixed_command_not_flagged_as_not_found() {
        // `GetSomethingCustom` starts with "Get" — treated as terminal, not CW226.
        let entry = make_entry_with_jomini(vec![vec![JominiCommand {
            key: "GetSomethingCustom".into(),
            params: Vec::new(),
        }]]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "Get-prefixed command should be accepted as terminal: {:?}",
            diags
        );
    }

    #[test]
    fn terminal_command_case_insensitive() {
        let entry = chain(&["Root", "getname"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "terminal lookup must be case-insensitive: {diags:?}"
        );
    }

    #[test]
    fn scripted_loc_with_format_suffix_is_accepted() {
        let entry = chain(&["Root", "AST_GetNavyName|Y"]);
        let mut data = hoi4_data_with_variables();
        data.scripted_locs = Some(&|name: &str| name.eq_ignore_ascii_case("AST_GetNavyName"));
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "|format suffix must be stripped before lookup: {diags:?}"
        );
    }

    #[test]
    fn scripted_loc_case_insensitive() {
        let entry = chain(&["Root", "ast_getnavyname"]);
        let mut data = hoi4_data_with_variables();
        data.scripted_locs = Some(&|name: &str| name.eq_ignore_ascii_case("AST_GetNavyName"));
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "scripted_loc lookup must be case-insensitive"
        );
    }

    #[test]
    fn scripted_loc_special_segments_are_trusted() {
        for seg in ["holder:foo", "$ARG$", "", "1.5"] {
            let entry = chain(&["Root", seg]);
            let mut data = hoi4_data_with_variables();
            data.scripted_locs = Some(&|_: &str| false);
            let diags = validate_loc_commands(&entry, ScopeId(100), &data);
            assert!(
                diags.is_empty(),
                "segment {seg:?} with :/$/numeric/empty must be trusted"
            );
        }
    }

    #[test]
    fn variable_with_format_and_mixed_case_is_accepted() {
        let entry = chain(&["?Root", "WAR_SUPPORT|1"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "variable lookup must strip |format and ignore case"
        );
    }

    #[test]
    fn command_chain_unknown_intermediate_with_terminal_tail_stays_lenient() {
        let entry = chain(&["Root", "PAL", "GetName"]);
        let data = hoi4_data_with_variables();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(
            diags.is_empty(),
            "unknown intermediate poisons even terminal tails"
        );
    }
}
