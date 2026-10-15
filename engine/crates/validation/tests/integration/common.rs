//! Fixtures shared by the validation integration tests.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use cwtools_game::constants::Game;
use cwtools_game::scope_registry::ScopeRegistry;
use cwtools_index::TypeIndex;
use cwtools_localization::LocIndex;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_rules::rules_types::{NewField, Options, RuleSet, RuleType};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{
    InlineScripts, Prepared, ValidationError, build_scope_registry_arc, validate_prepared,
};

/// What a test varies about one validation run. `Opts::DEFAULT` is the run
/// `validate_ast` makes of a bare `test.txt` with the scope and variable checks
/// on, without consulting the `CWTOOLS_NO_*_CHECKS` environment variables.
#[derive(Clone, Copy)]
pub struct Opts<'a> {
    pub path: &'a str,
    pub game: Option<Game>,
    pub type_index: Option<&'a TypeIndex>,
    pub modifier_keys: Option<&'a HashSet<String>>,
    pub loc_index: Option<&'a LocIndex>,
    pub extra_loc_keys: Option<&'a HashSet<String>>,
    pub inline_scripts: Option<&'a InlineScripts>,
    pub scope_checks: bool,
    pub var_checks: bool,
}

impl Opts<'static> {
    pub const DEFAULT: Self = Self {
        path: "test.txt",
        game: None,
        type_index: None,
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        scope_checks: true,
        var_checks: true,
    };
}

/// A type index with nothing in it, for the runs that need an index to exist.
pub static NO_TYPES: LazyLock<TypeIndex> = LazyLock::new(TypeIndex::new);

/// Parse the `.cwt` text `cwt` into a ruleset. The table comes back with it
/// because the script under test has to be parsed with the same one.
pub fn ruleset(cwt: &str) -> (StringTable, RuleSet) {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(cwt, &table), &table);
    (table, ruleset)
}

/// The one place a `Prepared` is spelled out. The registry is a parameter
/// because `Prepared` borrows it: build it with `build_scope_registry_arc`.
pub fn prepared<'a>(
    ruleset: &'a RuleSet,
    table: &'a StringTable,
    registry: Option<&'a Arc<ScopeRegistry>>,
    opts: &Opts<'a>,
) -> Prepared<'a> {
    let Opts {
        path: _,
        game,
        type_index,
        modifier_keys,
        loc_index,
        extra_loc_keys,
        inline_scripts,
        scope_checks,
        var_checks,
    } = *opts;
    Prepared {
        ruleset,
        table,
        game,
        type_index,
        modifier_keys,
        loc_index,
        extra_loc_keys,
        inline_scripts,
        registry,
        scope_checks,
        var_checks,
    }
}

/// Validate `script` against the rules in `cwt`.
pub fn validate(cwt: &str, script: &str, opts: &Opts) -> Vec<ValidationError> {
    let (table, ruleset) = ruleset(cwt);
    validate_with(&table, &ruleset, script, opts)
}

/// As [`validate`], for a test that already holds the ruleset and its table.
pub fn validate_with(
    table: &StringTable,
    ruleset: &RuleSet,
    script: &str,
    opts: &Opts,
) -> Vec<ValidationError> {
    let parsed = parse_string(script, table);
    let registry = build_scope_registry_arc(ruleset, opts.game);
    let prepared = prepared(ruleset, table, registry.as_ref(), opts);
    validate_prepared(&parsed, opts.path, &prepared)
}

/// The codes `validate` reports for `script`, in order.
pub fn codes(cwt: &str, script: &str, opts: &Opts) -> Vec<String> {
    error_codes(&validate(cwt, script, opts))
}

pub fn error_codes(errors: &[ValidationError]) -> Vec<String> {
    errors
        .iter()
        .filter_map(|e| e.code.map(String::from))
        .collect()
}

/// Position of `marker`'s first char in `script`: 1-based line, 0-based col
/// (the parser/LSP convention).
pub fn pos_of(script: &str, marker: &str) -> (u32, u16) {
    let off = script
        .find(marker)
        .unwrap_or_else(|| panic!("marker {:?} not in script", marker));
    let before = &script[..off];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().unwrap().len() as u16;
    (line, col)
}

/// The SpecificField keys in `rules`, for assertion messages.
pub fn specific_keys(rules: &[(RuleType, Options)]) -> Vec<String> {
    rules
        .iter()
        .filter_map(|(rt, _)| match rt {
            RuleType::LeafRule {
                left: NewField::SpecificField(k),
                ..
            }
            | RuleType::NodeRule {
                left: NewField::SpecificField(k),
                ..
            } => Some(k.clone()),
            _ => None,
        })
        .collect()
}
