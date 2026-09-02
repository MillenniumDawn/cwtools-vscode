use crate::commands::LocEntry;
use crate::loc_string::JominiCommand;
use cwtools_game::constants::Game as EngineGame;
use cwtools_game::scope_engine::{SCOPE_ANY, ScopeContext, ScopeId, ScopeResult};
use cwtools_game::scope_registry::ScopeRegistry;
use rustc_hash::FxHashSet;
use std::borrow::Cow;
use std::sync::Arc;

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

/// placeholder to satisfy its signature.
fn build_loc_ctx(data: &LocScopeData<'_>, initial: ScopeId) -> ScopeContext {
    match &data.registry {
        Some(reg) => ScopeContext::from_registry(reg.clone(), initial),
        None => ScopeContext::new(EngineGame::Hoi4, initial),
    }
}

pub fn validate_loc_commands(
    entry: &LocEntry,
    initial_scope: ScopeId,
    data: &LocScopeData<'_>,
) -> Vec<LocCommandDiagnostic> {
    if entry.commands.is_empty() && entry.jomini_commands.is_empty() {
        return Vec::new();
    }

    let mut diags = Vec::new();

    let terminal_set: &FxHashSet<String> = &data.terminal_commands;

    for cmd in &entry.commands {
        validate_command_string(cmd, initial_scope, data, terminal_set, &mut diags);
    }

    for chain in &entry.jomini_commands {
        validate_jomini_chain(chain, initial_scope, data, terminal_set, &mut diags);
    }

    diags
}

fn is_bypass_prefix(lower: &str, data: &LocScopeData<'_>) -> bool {
    lower.starts_with("event_target:")
        || lower.starts_with("scope:")
        || (data.parameter_variables && lower.starts_with("parameter:"))
        || (data.question_mark_variable && lower.starts_with('?'))
}

enum SegmentPre {
    Bypass,
    TerminalStop,
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

/// `Unknown` on the final segment and how they track lenient intermediates, so
enum ScopeOutcome {
    Advanced,
    /// `AnyScope` — advanced leniently (callers may note an "any" intermediate).
    AnyScope,
    ValueEnd,
    /// `ValueFound` mid-chain — lenient stop-continue (no scope progress, but the
    ValueMid,
    Wrong {
        command: String,
        current: ScopeId,
        expected: Vec<ScopeId>,
    },
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

        let seg_lower = seg.to_ascii_lowercase();

        let looks_terminal = match classify_segment(&seg_lower, is_last, data, terminal_set) {
            SegmentPre::Bypass => {
                ctx.push_scope(SCOPE_ANY);
                continue;
            }
            SegmentPre::TerminalStop => break,
            SegmentPre::ScopeChange { looks_terminal } => looks_terminal,
        };

        match classify_scope_result(ctx.change_scope(seg), is_last) {
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
                return;
            }
            ScopeOutcome::Unknown => {
                // terminal-commands list, accept it (lenient); if we have a
                // not scope links, so we remain lenient here.
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
    // has an opaque intermediate value and stays lenient. Other multi-segment
    let marker = chain[0].key.strip_prefix('?');
    let has_q_mark = data.question_mark_variable && marker.is_some();
    let reads_through_variable = has_q_mark && marker.is_some_and(|m| reads_a_variable(m, data));
    let lacks_variable_registry = has_q_mark && data.scripted_variables.is_none();
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

        let seg_lower = seg.to_ascii_lowercase();

        let looks_terminal = match classify_segment(&seg_lower, is_last, data, terminal_set) {
            SegmentPre::Bypass => {
                ctx.push_scope(SCOPE_ANY);
                if !is_last && !seg_lower.starts_with('?') {
                    had_lenient_intermediate = true;
                }
                continue;
            }
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

/// which the registry never holds; strip it before asking. Three forms are taken
fn reads_a_variable(segment: &str, data: &LocScopeData<'_>) -> bool {
    is_known_name(segment, data.scripted_variables)
}

fn is_scripted_loc(segment: &str, data: &LocScopeData<'_>) -> bool {
    is_known_name(segment, data.scripted_locs)
}

fn is_terminal_command(lower: &str, terminal_set: &FxHashSet<String>) -> bool {
    lower.starts_with("get") || terminal_set.contains(lower)
}

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
            // is a different answer (see the lenient tests below).
            scripted_locs: Some(&|_: &str| false),
            scripted_guis: Some(&|_: &str| false),
        }
    }

    fn hoi4_data_with_variables() -> LocScopeData<'static> {
        LocScopeData {
            scripted_variables: Some(&|name: &str| name.eq_ignore_ascii_case("war_support")),
            ..hoi4_data()
        }
    }

    #[test]
    fn valid_chain_state_owner_getname() {
        let entry = make_entry_with_commands(vec!["owner.GetName".into()]);
        let data = hoi4_data();

        let diags = validate_loc_commands(&entry, ScopeId(101), &data);
        assert!(
            diags.is_empty(),
            "owner.GetName from State scope should be valid, got: {:?}",
            diags
        );
    }

    #[test]
    fn invalid_chain_country_controller_wrong_scope() {
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

    #[test]
    fn event_target_bypass() {
        let entry = make_entry_with_commands(vec!["event_target:my_target".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "event_target: should always be accepted");
    }

    #[test]
    fn parameter_bypass() {
        let entry = make_entry_with_commands(vec!["parameter:my_param".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "parameter: should always be accepted");
    }

    #[test]
    fn question_mark_variable_bypass() {
        let entry = make_entry_with_commands(vec!["?some_var".into()]);
        let data = hoi4_data();
        let diags = validate_loc_commands(&entry, ScopeId(100), &data);
        assert!(diags.is_empty(), "?variable should always be accepted");
    }

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

    #[test]
    fn jomini_getname_accepted() {
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

    #[test]
    fn jomini_wrong_scope_controller_from_country() {
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

    #[test]
    fn jomini_chain_state_owner_getname() {
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

    #[test]
    fn unknown_final_command_with_registry_emits_not_found() {
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
