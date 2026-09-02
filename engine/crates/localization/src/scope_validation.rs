use crate::commands::LocEntry;
use crate::loc_string::JominiCommand;
use cwtools_game::constants::Game as EngineGame;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId, ScopeResult};
use cwtools_game::scope_registry::ScopeRegistry;
use rustc_hash::FxHashSet;
use std::borrow::Cow;
use std::sync::Arc;

/// Diagnostic from loc command validation.
#[derive(Debug, Clone, PartialEq)]
pub enum LocCommandDiagnostic {
    WrongScope {
        command: String,
        current_scope: u32,
        expected_scopes: Vec<u32>,
    },
    ChainEndsInScope {
        command: String,
    },
    NotFound {
        command: String,
    },
    ScriptedGuiNotFound {
        callback: String,
    },
}

// Sync because loc validation fans out over rayon holding &LocScopeData.
pub type ScriptedVariables<'a> = &'a (dyn Fn(&str) -> bool + Sync);

pub struct LocScopeData<'a> {
    pub game: Option<EngineGame>,
    pub terminal_commands: Cow<'a, FxHashSet<String>>,
    pub question_mark_variable: bool,
    pub parameter_variables: bool,
    pub registry: Option<Arc<ScopeRegistry>>,
    // None keeps chains lenient (#348): no data cannot judge typo.
    pub scripted_variables: Option<ScriptedVariables<'a>>,
    pub scripted_locs: Option<ScriptedVariables<'a>>,
    pub scripted_guis: Option<ScriptedVariables<'a>>,
}

impl Default for LocScopeData<'_> {
    fn default() -> Self {
        Self {
            game: None,
            terminal_commands: Cow::Owned(FxHashSet::default()),
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
/// with the validation path); without one, `ScopeContext::new` builds an empty
/// registry regardless of the `Game` passed in, so the constant here is just a
/// placeholder to satisfy its signature.
fn build_loc_ctx(data: &LocScopeData<'_>, initial: ScopeId) -> ScopeContext {
    match &data.registry {
        Some(reg) => ScopeContext::from_registry(reg.clone(), initial),
        None => ScopeContext::new(EngineGame::Hoi4, initial),
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
    // Nothing to validate: skip building the terminal set and return the empty
    // (non-allocating) Vec for the common no-command entry.
    if entry.commands.is_empty() && entry.jomini_commands.is_empty() {
        return Vec::new();
    }

    let mut diags = Vec::new();

    // `terminal_commands` is already lowercased (from RuleSet). Use directly.
    let terminal_set: &FxHashSet<String> = &data.terminal_commands;

    // Validate legacy [command] strings (single-segment, dot-split internally)
    for cmd in &entry.commands {
        validate_command_string(cmd, initial_scope, data, terminal_set, &mut diags);
    }

    // Validate Jomini command chains — each inner Vec is one bracket's chain.
    for chain in &entry.jomini_commands {
        validate_jomini_chain(chain, initial_scope, data, terminal_set, &mut diags);
    }

    diags
}

// ── Internal helpers ──────────────────────────────────────────────────────────

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
    data: &LocScopeData<'_>,
    terminal_set: &FxHashSet<String>,
    diags: &mut Vec<LocCommandDiagnostic>,
) {
    if is_bypass_prefix(&cmd.to_ascii_lowercase(), data) {
        return;
    }

    let segments: Vec<&str> = cmd.split('.').collect();
    let last_idx = segments.len().saturating_sub(1);

    let mut ctx = build_loc_ctx(data, initial_scope);

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
    data: &LocScopeData<'_>,
    terminal_set: &FxHashSet<String>,
    diags: &mut Vec<LocCommandDiagnostic>,
) {
    if chain.is_empty() || chain.iter().any(|cmd| cmd.key.is_empty()) {
        return;
    }
    let last_idx = chain.len() - 1;
    let mut ctx = build_loc_ctx(data, initial_scope);
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
            terminal_commands: Cow::Owned(
                ["GetName", "GetNameDef", "GetAdjective", "GetLeader"]
                    .into_iter()
                    .map(|s| s.to_ascii_lowercase())
                    .collect(),
            ),
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
            terminal_commands: Cow::Owned(FxHashSet::default()),
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

    // ── #339: loc scope checks reject a chain used from the wrong scope ──────
    // (config-registry path — CK2 no longer carries a hardcoded scope table)

    /// A minimal CK2-shaped registry: Character and Title scopes, linked by
    /// `primary_title` (valid only from Character).
    fn ck2_registry() -> ScopeRegistry {
        use cwtools_game::scope_registry::{LinkInput, ScopeInput};
        ScopeRegistry::from_config(
            &[
                ScopeInput {
                    name: "Character".to_string(),
                    aliases: vec!["character".to_string()],
                    is_subscope_of: Vec::new(),
                },
                ScopeInput {
                    name: "Title".to_string(),
                    aliases: vec!["title".to_string()],
                    is_subscope_of: Vec::new(),
                },
            ],
            &[LinkInput {
                name: "primary_title".to_string(),
                output_scope: Some("title".to_string()),
                input_scopes: vec!["character".to_string()],
                prefix: None,
                from_data: false,
                data_source: Vec::new(),
            }],
            EngineGame::Ck2,
        )
    }

    #[test]
    fn ck2_config_registry_chain_valid_from_character_scope() {
        // Starting in Character: primary_title → Title → GetName.
        let reg = ck2_registry();
        let character = reg
            .id_of("character")
            .expect("character scope in test registry");
        let entry = make_entry_with_commands(vec!["primary_title.GetName".into()]);
        let data = LocScopeData {
            game: Some(EngineGame::Ck2),
            registry: Some(Arc::new(reg)),
            ..LocScopeData::default()
        };

        let diags = validate_loc_commands(&entry, character, &data);
        assert!(
            diags.is_empty(),
            "primary_title.GetName from Character scope should be valid, got: {diags:?}"
        );
    }

    #[test]
    fn ck2_config_registry_chain_rejected_from_title_scope() {
        // Starting in Title: `primary_title` is only valid from Character.
        let reg = ck2_registry();
        let title = reg.id_of("title").expect("title scope in test registry");
        let entry = make_entry_with_commands(vec!["primary_title.GetName".into()]);
        let data = LocScopeData {
            game: Some(EngineGame::Ck2),
            registry: Some(Arc::new(reg)),
            ..LocScopeData::default()
        };

        let diags = validate_loc_commands(&entry, title, &data);
        assert!(
            matches!(diags.as_slice(), [LocCommandDiagnostic::WrongScope { .. }]),
            "primary_title from Title scope must be rejected, got: {diags:?}"
        );
    }
}
