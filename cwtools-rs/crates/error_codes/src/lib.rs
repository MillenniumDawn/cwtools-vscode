//! Shared error-code catalog for cwtools.
//!
//! Depends only on `cwtools_i18n` (itself a leaf) so both `cwtools_validation`
//! and `cwtools_localization` can use the same CW### codes without a dependency
//! cycle (validation depends on localization).
//!
//! Every message here is the English one. The translations live in
//! [`mod@translations`] beside it, and [`ErrorCode::message`] picks the one for
//! the locale the LSP was started in; nothing sets a locale in the CLI, so
//! batch runs and the corpus baselines stay English.

mod translations;

/// Severity of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Structured error code catalog matching F# CWTools error codes.
/// Each code has a fixed ID (CW###), a severity level, and a message template.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorCode {
    pub id: &'static str,
    pub severity: ErrorSeverity,
    pub message_template: &'static str,
}

/// The published error-code reference, which every diagnostic links back to.
pub const DOC_URL: &str =
    "https://github.com/MillenniumDawn/cwtools/blob/main/cwtools-rs/docs/ERROR_CODES.md";

/// Link straight to `id`'s row in the error-code reference, for a reader who
/// wants the long form of what a `CWxxx` means.
///
/// The anchor is the lowercased id; `docs/ERROR_CODES.md` carries one
/// `<a id="cw###">` target per row, so the fragment survives edits to the
/// section headings around it.
///
/// # Examples
///
/// ```
/// let url = cwtools_error_codes::doc_url("CW113");
/// assert!(url.ends_with("ERROR_CODES.md#cw113"));
/// ```
pub fn doc_url(id: &str) -> String {
    format!("{DOC_URL}#{}", id.to_ascii_lowercase())
}

/// CW223's HOI4 wording ([`CW223_INCORRECT_NOT_USAGE_HOI4_MSG`]) in the active
/// locale. The pair is one diagnostic with two English messages, so the second
/// one needs the same treatment [`ErrorCode::message`] gives the first.
pub fn cw223_hoi4_message() -> &'static str {
    translations::template(translations::CW223_HOI4_KEY)
        .unwrap_or(CW223_INCORRECT_NOT_USAGE_HOI4_MSG)
}

impl ErrorCode {
    /// This code's template in the active locale, falling back to the English
    /// `message_template` for a code that locale hasn't translated. Use it
    /// wherever a template is read directly rather than through
    /// [`format`](Self::format); `message_template` itself stays English, which
    /// is what the reference doc and the CLI's `--explain` want.
    pub fn message(&self) -> &'static str {
        translations::template(self.id).unwrap_or(self.message_template)
    }

    /// Substitute each `{}` placeholder in the template with the next param,
    /// in order (positional, like `format!`). Extra `{}` are left as-is.
    pub fn format(&self, params: &[impl AsRef<str>]) -> String {
        let template = self.message();
        let mut result = String::with_capacity(template.len());
        let mut it = params.iter();
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' && chars.peek() == Some(&'}') {
                chars.next(); // consume '}'
                match it.next() {
                    Some(p) => result.push_str(p.as_ref()),
                    None => result.push_str("{}"),
                }
            } else {
                result.push(c);
            }
        }
        result
    }
}

// ── Error Code Catalog ─────────────────────────────────

/// Localisation file parse error.
///
/// F# emits this from `validateLocalisationSyntax` when `YAMLLocalisationParser`
/// returns `Failure(msg, pos, _)`. The Rust parser is lenient (recovers
/// line-by-line), so this fires at the recovery point for each malformed line.
pub const CW001_PARSE_ERROR: ErrorCode = ErrorCode {
    id: "CW001",
    severity: ErrorSeverity::Error,
    message_template: "Localisation file parse error: {}",
};

/// Missing localisation key.
pub const CW100_MISSING_LOCALISATION: ErrorCode = ErrorCode {
    id: "CW100",
    severity: ErrorSeverity::Warning,
    message_template: "Localisation key {} is not defined for {}",
};

/// Trigger used in wrong scope. F# `IncorrectTriggerScope`.
pub const CW104_INCORRECT_TRIGGER_SCOPE: ErrorCode = ErrorCode {
    id: "CW104",
    severity: ErrorSeverity::Error,
    message_template: "{} trigger used in incorrect scope. In {} but expected {}",
};

/// Localisation key quoted when used inline.
pub const CW122_LOC_KEY_IN_INLINE: ErrorCode = ErrorCode {
    id: "CW122",
    severity: ErrorSeverity::Information,
    message_template: "Localisation key {} should not be quoted when used inline, this can cause unexpected behaviour",
};

// ── Rules-engine dynamic codes (F# CWTools/Rules/*) ─────
//
// These replace the Rust-invented CW200-205. F# emits a node-kind-specific code
// for a structural mismatch (CW262/263/264/265), one code for any cardinality
// violation (CW242, covering both under- and over-count), and one for a wrong
// value (CW240). The severity/message are computed at the emission site (F#
// threads them from the rule's `## severity` option), so the const severity
// here is the documented default, not a hard rule.

/// A value didn't match its rule's field type (int/float/enum/bool/date/…).
/// F# `ConfigRulesUnexpectedValue`. Replaces the Rust-invented CW202/CW205.
pub const CW240_UNEXPECTED_VALUE: ErrorCode = ErrorCode {
    id: "CW240",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// Cardinality violation — a field appears too few or too many times.
/// F# `ConfigRulesWrongNumber`. Replaces the Rust-invented CW203/CW204.
pub const CW242_WRONG_NUMBER: ErrorCode = ErrorCode {
    id: "CW242",
    severity: ErrorSeverity::Warning,
    message_template: "{}",
};

/// A target's scope doesn't match the expected scope.
/// F# `ConfigRulesTargetWrongScope`.
pub const CW243_TARGET_WRONG_SCOPE: ErrorCode = ErrorCode {
    id: "CW243",
    severity: ErrorSeverity::Error,
    message_template: "Target \"{}\" has incorrect scope. Is {} but expect {}",
};

/// A value isn't a valid target. F# `ConfigRulesInvalidTarget`.
pub const CW244_INVALID_TARGET: ErrorCode = ErrorCode {
    id: "CW244",
    severity: ErrorSeverity::Error,
    message_template: "{} is not a target. Expected a target in scope(s) {}",
};

/// A scope link inside a target chain was used in the wrong scope.
/// F# `ConfigRulesErrorInTarget`.
pub const CW245_ERROR_IN_TARGET: ErrorCode = ErrorCode {
    id: "CW245",
    severity: ErrorSeverity::Error,
    message_template: "Error in target. Link {} was used in scope {} but expected {}",
};

/// A referenced variable was never set. F# `ConfigRulesUnsetVariable`.
pub const CW246_UNSET_VARIABLE: ErrorCode = ErrorCode {
    id: "CW246",
    severity: ErrorSeverity::Warning,
    message_template: "The variable {} has not been set",
};

/// A trigger/effect/modifier rule was used in the wrong scope.
/// F# `ConfigRulesRuleWrongScope`.
pub const CW247_RULE_WRONG_SCOPE: ErrorCode = ErrorCode {
    id: "CW247",
    severity: ErrorSeverity::Error,
    message_template: "Trigger/Effect/Modifier {} used in wrong scope. In {} but expect {}",
};

/// An invalid scope command. F# `ConfigRulesInvalidScopeCommand`.
pub const CW248_INVALID_SCOPE_COMMAND: ErrorCode = ErrorCode {
    id: "CW248",
    severity: ErrorSeverity::Error,
    message_template: "Invalid scope command {}",
};

/// An unexpected `key = { ... }` node. F# `ConfigRulesUnexpectedPropertyNode`.
/// Replaces one arm of the Rust-invented CW201.
pub const CW262_UNEXPECTED_PROPERTY_NODE: ErrorCode = ErrorCode {
    id: "CW262",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// An unexpected `key = value` leaf. F# `ConfigRulesUnexpectedPropertyLeaf`.
/// Replaces one arm of the Rust-invented CW201.
pub const CW263_UNEXPECTED_PROPERTY_LEAF: ErrorCode = ErrorCode {
    id: "CW263",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// An unexpected bare value. F# `ConfigRulesUnexpectedPropertyLeafValue`.
/// Replaces one arm of the Rust-invented CW201.
pub const CW264_UNEXPECTED_PROPERTY_LEAF_VALUE: ErrorCode = ErrorCode {
    id: "CW264",
    severity: ErrorSeverity::Warning,
    message_template: "{}",
};

/// An unexpected `{ ... }` value clause.
/// F# `ConfigRulesUnexpectedPropertyValueClause`.
/// Replaces one arm of the Rust-invented CW201.
pub const CW265_UNEXPECTED_PROPERTY_VALUE_CLAUSE: ErrorCode = ErrorCode {
    id: "CW265",
    severity: ErrorSeverity::Warning,
    message_template: "{}",
};

/// An alias key/value didn't match the expected alias category.
/// F# `ConfigRulesUnexpectedAliasKeyValue`.
pub const CW267_UNEXPECTED_ALIAS_KEY_VALUE: ErrorCode = ErrorCode {
    id: "CW267",
    severity: ErrorSeverity::Error,
    message_template: "Expected a {} value, got {}",
};

/// A value has more precision than the engine supports here.
/// F# `ConfigRulesVariableTooSmall`.
pub const CW270_VARIABLE_TOO_SMALL: ErrorCode = ErrorCode {
    id: "CW270",
    severity: ErrorSeverity::Warning,
    message_template: "Value too small, only 3 decimal places are supported in this context",
};

/// An integer was expected. F# `ConfigRulesVariableIntOnly`.
pub const CW271_VARIABLE_INT_ONLY: ErrorCode = ErrorCode {
    id: "CW271",
    severity: ErrorSeverity::Warning,
    message_template: "Expected an integer",
};

/// A custom error attached to a rule (`## error = ...`). F# `FromRulesCustomError`.
pub const CW272_FROM_RULES_CUSTOM_ERROR: ErrorCode = ErrorCode {
    id: "CW272",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// An `inline_script` call whose body the validator could not pull in: the
/// script doesn't exist, the call names none, or the chain loops or runs too
/// deep. F# `InlineScriptResultsInError`.
pub const CW274_INLINE_SCRIPT_ERROR: ErrorCode = ErrorCode {
    id: "CW274",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// Localisation string references another key that doesn't exist.
pub const CW225_UNDEFINED_LOC_REFERENCE: ErrorCode = ErrorCode {
    id: "CW225",
    severity: ErrorSeverity::Error,
    message_template: "Localisation key \"{}\" references \"{}\" which doesn't exist in {}",
};

/// Localisation string uses a command that doesn't exist.
pub const CW226_INVALID_LOC_COMMAND: ErrorCode = ErrorCode {
    id: "CW226",
    severity: ErrorSeverity::Error,
    message_template: "Localisation key \"{}\" uses command \"{}\" which doesn't exist",
};

/// Localisation value is a placeholder (REPLACE_ME / TODO_CD).
pub const CW234_REPLACE_ME_LOC: ErrorCode = ErrorCode {
    id: "CW234",
    severity: ErrorSeverity::Information,
    message_template: "Localisation key {} is a placeholder for {}",
};

/// Localisation file is not UTF-8 with BOM.
pub const CW254_WRONG_ENCODING: ErrorCode = ErrorCode {
    id: "CW254",
    severity: ErrorSeverity::Error,
    message_template: "Localisation files must be UTF-8 BOM, this file is not",
};

/// Localisation file name carries no recognised `l_xxx` language tag.
pub const CW255_MISSING_LOC_FILE_LANG: ErrorCode = ErrorCode {
    id: "CW255",
    severity: ErrorSeverity::Error,
    message_template: "Localisation file name should contain (and ideally end with) \"l_language.yml\"",
};

/// Localisation file's first line is not a recognised `l_xxx:` header.
pub const CW256_MISSING_LOC_FILE_LANG_HEADER: ErrorCode = ErrorCode {
    id: "CW256",
    severity: ErrorSeverity::Error,
    message_template: "Localisation file should start with \"l_language:\" on the first line (or a comment)",
};

/// Localisation file name's language and the header language disagree.
pub const CW257_LOC_FILE_LANG_MISMATCH: ErrorCode = ErrorCode {
    id: "CW257",
    severity: ErrorSeverity::Error,
    message_template: "Localisation file's name has language {} doesn't match the header language {}",
};

/// Localisation file name's language tag is not at the end of the name.
///
/// F# defines this (`LocFileLangWrongPlace`) but leaves the emission commented
/// out as "only convention", so cwtools-rs keeps the const for parity but never
/// fires it. See `STLLocalisationString.checkLocFileName`.
pub const CW258_LOC_FILE_LANG_WRONG_PLACE: ErrorCode = ErrorCode {
    id: "CW258",
    severity: ErrorSeverity::Information,
    message_template: "Localisation file name should end with \"l_language.yml\"",
};

/// Localisation string refers to itself.
pub const CW259_RECURSIVE_LOC_REF: ErrorCode = ErrorCode {
    id: "CW259",
    severity: ErrorSeverity::Error,
    message_template: "This localisation string refers to itself",
};

/// Localisation command used in the wrong scope.
pub const CW260_LOC_COMMAND_WRONG_SCOPE: ErrorCode = ErrorCode {
    id: "CW260",
    severity: ErrorSeverity::Error,
    message_template: "Loc command {} used in wrong scope. In {} but expected {}",
};

/// Key of a `unique` type is defined more than once.
///
/// Reconciliation: cwtools-rs originally emitted this as the Rust-invented
/// `CW501`. F# assigns it `CW261` (`DuplicateTypeDef`); converging on the F# ID
/// since downstream baselines key off CW numbers and the F# binary is going
/// away. Args are `(id, typename)`.
pub const CW261_DUPLICATE_TYPE_DEF: ErrorCode = ErrorCode {
    id: "CW261",
    severity: ErrorSeverity::Error,
    message_template: "Key {} of type {} is defined multiple times",
};

/// Localisation command not valid in the resolved data type.
///
/// Reconciliation: cwtools-rs originally emitted this as `CW262`, but that ID
/// belongs to F#'s rules-engine `ConfigRulesUnexpectedPropertyNode`. The
/// message here matches F#'s `LocCommandNotInDataType`, which is `CW266`.
pub const CW266_LOC_COMMAND_NOT_IN_DATA_TYPE: ErrorCode = ErrorCode {
    id: "CW266",
    severity: ErrorSeverity::Error,
    message_template: "Localisation key {} uses command {} which does not exist in data type {}.",
};

/// Localisation value doesn't start and end with double quotes.
pub const CW268_LOC_MISSING_QUOTE: ErrorCode = ErrorCode {
    id: "CW268",
    severity: ErrorSeverity::Warning,
    message_template: "Localisation key {} doesn't start and end with double quotes",
};

/// Localisation value contains unexpected characters.
pub const CW275_LOC_INVALID_CHARS: ErrorCode = ErrorCode {
    id: "CW275",
    severity: ErrorSeverity::Warning,
    message_template: "Localisation value for {} contains unexpected characters, and may not render correctly",
};

/// Localisation key contains spaces or characters not valid in loc keys.
pub const CW276_LOC_KEY_INVALID_CHARS: ErrorCode = ErrorCode {
    id: "CW276",
    severity: ErrorSeverity::Warning,
    message_template: "Localisation key {} contains invalid characters (spaces or special characters are not allowed)",
};

/// Validation stopped after an alias-overload branch budget was exhausted.
pub const CW277_ALIAS_BRANCH_LIMIT: ErrorCode = ErrorCode {
    id: "CW277",
    severity: ErrorSeverity::Warning,
    message_template: "Validation stopped after reaching the alias branch limit",
};

/// Event may fire every tick (performance hint). F# `EventEveryTick`.
///
/// Reconciliation: cwtools-rs originally emitted this as the Rust-invented
/// `CW300` at `Warning`. F# assigns it `CW107` at `Information` (it's a perf
/// hint, not a defect). The message keeps the more specific cwtools-rs wording
/// (it also lists the `trigger={always=no}` escape hatch the check honours).
pub const CW107_EVENT_EVERY_TICK: ErrorCode = ErrorCode {
    id: "CW107",
    severity: ErrorSeverity::Information,
    message_template: "Event is missing mean_time_to_happen, is_triggered_only, fire_only_once, or trigger={always=no}. Performance concern: event may fire every tick.",
};

// CW301 (pre-trigger at wrong level) retired: it was Rust-only and duplicated
// F#'s CW120 (`PossiblePretrigger`) on the same leaf; CW120 is the single
// pretrigger-placement diagnostic.

// ── Tier B — boolean/syntax structural hints (cross-game) ──────────────────
//
// Pure AST-pattern checks ported from F# CWTools/Validation/Common/
// CommonValidation.fs. Emitted from `per_game::structural`, which walks every
// `key = { ... }` block (this parser stores those as Node OR Leaf-with-Clause)
// and matches on the reserved logic keywords.

/// An `if`/`else_if` block that contains no effects (only `limit`, or nothing).
/// F# `EmptyIf`.
pub const CW121_EMPTY_IF: ErrorCode = ErrorCode {
    id: "CW121",
    severity: ErrorSeverity::Warning,
    message_template: "This 'if' trigger contains no effects",
};

/// A `NOT` block with more than one child. F# `IncorrectNotUsage`.
///
/// The default message suits games with `NOR`/`NAND` triggers (Stellaris, EU4).
/// HOI4 has neither — use [`CW223_INCORRECT_NOT_USAGE_HOI4_MSG`] there.
pub const CW223_INCORRECT_NOT_USAGE: ErrorCode = ErrorCode {
    id: "CW223",
    severity: ErrorSeverity::Information,
    message_template: "Do not use NOT with multiple children, replace this with either NOR or NAND to avoid ambiguity",
};

/// CW223 message for HOI4, where `NOR`/`NAND` are not valid triggers. `NOT` with
/// multiple children is well-defined (logical NOR); the only ambiguity is for
/// readers, so the fix is to make the intent explicit with `OR`/`AND`.
pub const CW223_INCORRECT_NOT_USAGE_HOI4_MSG: &str = "NOT with multiple children acts as NOR (true only if every child is false). Make intent explicit: NOT = { OR = { ... } } for NOR, or NOT = { AND = { ... } } for NAND.";

/// A boolean operator nested directly inside the same operator (`AND` in `AND`,
/// `OR` in `OR`). F# `UnnecessaryBoolean`. Arg is the operator name.
pub const CW251_UNNECESSARY_BOOLEAN: ErrorCode = ErrorCode {
    id: "CW251",
    severity: ErrorSeverity::Warning,
    message_template: "This {} is unnecessary",
};

/// A field whose body is exactly `{ always = <bool> }` where that bool matches
/// the game default, so the whole field is a no-op and can be deleted. A
/// Rust-original cleanup hint (no F# equivalent); the field/default table lives
/// in `per_game::hoi4`. Arg is the field name.
pub const CW280_REDUNDANT_DEFAULT_FIELD: ErrorCode = ErrorCode {
    id: "CW280",
    severity: ErrorSeverity::Information,
    message_template: "{} = { always = ... } matches the default and can be removed",
};

/// A `limit = { }` block with no trigger conditions. An empty limit matches
/// everything, so it is almost always a mistake (forgotten conditions) or dead
/// weight. A Rust-original structural hint (no F# equivalent).
pub const CW281_EMPTY_LIMIT: ErrorCode = ErrorCode {
    id: "CW281",
    severity: ErrorSeverity::Warning,
    message_template: "This 'limit' contains no triggers",
};

/// A bool field is explicitly set to the engine default declared by the rule's
/// `## default_bool = yes|no` directive, so the line is redundant and can be
/// omitted. A Rust-original cleanup hint (no F# equivalent). Arg is the value.
pub const CW282_REDUNDANT_DEFAULT_BOOL: ErrorCode = ErrorCode {
    id: "CW282",
    severity: ErrorSeverity::Information,
    message_template: "This is the default value ({}) and can be omitted",
};

/// A `[!name]` localisation call names no indexed scripted-GUI callback.
pub const CW283_SCRIPTED_GUI_CALLBACK_NOT_FOUND: ErrorCode = ErrorCode {
    id: "CW283",
    severity: ErrorSeverity::Error,
    message_template: "Localisation key \"{}\" calls scripted GUI callback \"{}\" which does not exist",
};

// ── Tier B — Stellaris-specific if/else + set_name (per_game::stellaris) ────

/// Nested `if`/`else` in effects, deprecated with Stellaris 2.1. F# `DeprecatedElse`.
pub const CW236_DEPRECATED_ELSE: ErrorCode = ErrorCode {
    id: "CW236",
    severity: ErrorSeverity::Warning,
    message_template: "Nested if/else in effects was deprecated with 2.1 and will be removed in a future release",
};

/// Ambiguous `if = { if else }` after the Stellaris 2.1 behaviour change.
/// F# `AmbiguousIfElse`.
pub const CW237_AMBIGUOUS_IF_ELSE: ErrorCode = ErrorCode {
    id: "CW237",
    severity: ErrorSeverity::Information,
    message_template: "2.1 changed nested if = { if else } behaviour in effects. Check this still works as expected",
};

/// An `else`/`else_if` that has no preceding `if`. F# `IfElseOrder`.
pub const CW238_IF_ELSE_ORDER: ErrorCode = ErrorCode {
    id: "CW238",
    severity: ErrorSeverity::Error,
    message_template: "An else/else_if is missing a preceding if",
};

/// `set_empire_name`/`set_planet_name` should be `set_name`. F# `DeprecatedSetName`.
pub const CW253_DEPRECATED_SET_NAME: ErrorCode = ErrorCode {
    id: "CW253",
    severity: ErrorSeverity::Information,
    message_template: "Consider using \"set_name\" instead for consistency",
};

// ── Tiers S/E/G — defined, emission blocked on missing subsystems ──────────
//
// These complete the F# catalog (Validation.fs) so the only remaining gap is
// the emission site, not the code definition. Each is annotated with the
// subsystem it needs. NONE are emitted yet — wiring them without that machinery
// would false-positive on valid game config, which the project forbids.

// -- Tier S: trigger/effect/scope-command wrong-scope. CW104/105/106 are wired
//    and ON by default (config-driven scope engine); see lib.rs scope checks.

/// Effect used in the wrong scope. F# `IncorrectEffectScope`.
pub const CW105_INCORRECT_EFFECT_SCOPE: ErrorCode = ErrorCode {
    id: "CW105",
    severity: ErrorSeverity::Error,
    message_template: "{} effect used in incorrect scope. In {} but expected {}",
};

/// Scope command used in the wrong scope. F# `IncorrectScopeScope`.
pub const CW106_INCORRECT_SCOPE_SCOPE: ErrorCode = ErrorCode {
    id: "CW106",
    severity: ErrorSeverity::Error,
    message_template: "{} scope command used in incorrect scope. In {} but expected {}",
};

/// Trigger that could be a pretrigger. F# `PossiblePretrigger`. Needs the
/// pretrigger registry + scope engine.
pub const CW120_POSSIBLE_PRETRIGGER: ErrorCode = ErrorCode {
    id: "CW120",
    severity: ErrorSeverity::Information,
    message_template: "Trigger {} can be made a pretrigger (see code action to fix)",
};

/// Modifier type referenced but not defined. F# `UndefinedModifierTypeForModifier`.
/// Needs a modifier-type registry.
pub const CW273_UNDEFINED_MODIFIER_TYPE: ErrorCode = ErrorCode {
    id: "CW273",
    severity: ErrorSeverity::Warning,
    message_template: "Modifier type {} is not defined but is used",
};

// -- Tier E: event-target dataflow + cross-file event index.

/// Required event target not set. F# `UnsavedEventTarget`. Needs event-target dataflow.
pub const CW220_UNSAVED_EVENT_TARGET: ErrorCode = ErrorCode {
    id: "CW220",
    severity: ErrorSeverity::Error,
    message_template: "{} or an event it calls require the event target(s) {} but they are not set by this event or by all possible events leading here",
};

/// Event target possibly not set. F# `MaybeUnsavedEventTarget`.
pub const CW221_MAYBE_UNSAVED_EVENT_TARGET: ErrorCode = ErrorCode {
    id: "CW221",
    severity: ErrorSeverity::Warning,
    message_template: "{} or an event it calls require the event target(s) {} but they may not always be set by this event or by all possible events leading here",
};

/// Reference to an undefined event id. F# `UndefinedEvent`. Needs a cross-file event index.
pub const CW222_UNDEFINED_EVENT: ErrorCode = ErrorCode {
    id: "CW222",
    severity: ErrorSeverity::Warning,
    message_template: "The event id {} is not defined",
};

// -- Tier G: game-specific, need vanilla data / cross-file registries.

/// research_leader missing `area`. F# `ResearchLeaderArea`.
pub const CW108_RESEARCH_LEADER_AREA: ErrorCode = ErrorCode {
    id: "CW108",
    severity: ErrorSeverity::Error,
    message_template: "This research_leader is missing required \"area\"",
};

/// research_leader area disagrees with the technology. F# `ResearchLeaderTech`.
pub const CW109_RESEARCH_LEADER_TECH: ErrorCode = ErrorCode {
    id: "CW109",
    severity: ErrorSeverity::Information,
    message_template: "This research_leader uses area {} but the technology uses area {}",
};

/// Technology has no category. F# `TechCatMissing`.
pub const CW110_TECH_CAT_MISSING: ErrorCode = ErrorCode {
    id: "CW110",
    severity: ErrorSeverity::Error,
    message_template: "No category found for this technology",
};

/// Referenced file not found (case-sensitive). F# `MissingFile`. Needs the file index.
pub const CW113_MISSING_FILE: ErrorCode = ErrorCode {
    id: "CW113",
    severity: ErrorSeverity::Error,
    message_template: "File {} not found, this is case sensitive",
};

/// Section template not found. F# `UnknownSectionTemplate`.
pub const CW227_UNKNOWN_SECTION_TEMPLATE: ErrorCode = ErrorCode {
    id: "CW227",
    severity: ErrorSeverity::Error,
    message_template: "Section template {} can not be found",
};

/// Section template has no such slot. F# `MissingSectionSlot`.
pub const CW228_MISSING_SECTION_SLOT: ErrorCode = ErrorCode {
    id: "CW228",
    severity: ErrorSeverity::Error,
    message_template: "Section template {} does not have a slot {}",
};

/// Component template not found. F# `UnknownComponentTemplate`.
pub const CW229_UNKNOWN_COMPONENT_TEMPLATE: ErrorCode = ErrorCode {
    id: "CW229",
    severity: ErrorSeverity::Error,
    message_template: "Component template {} can not be found",
};

/// Component and slot size mismatch. F# `MismatchedComponentAndSlot`.
pub const CW230_MISMATCHED_COMPONENT_AND_SLOT: ErrorCode = ErrorCode {
    id: "CW230",
    severity: ErrorSeverity::Warning,
    message_template: "Component and slot do not match, slot {} has size {} and component {} has size {}",
};

/// Technology is never used. F# `UnusedTech`. Rides the project-wide reference
/// map CW239 uses (`validation::references`); the exemptions F# applies first
/// live in the Stellaris validator.
pub const CW231_UNUSED_TECH: ErrorCode = ErrorCode {
    id: "CW231",
    severity: ErrorSeverity::Warning,
    message_template: "Technology {} is not used",
};

/// Section entity not defined. F# `UndefinedEntity`/`UndefinedSectionEntity`.
pub const CW233_UNDEFINED_ENTITY: ErrorCode = ErrorCode {
    id: "CW233",
    severity: ErrorSeverity::Error,
    message_template: "Entity {} is not defined",
};

/// Modifier with value 0 (additive, so a no-op). F# `ZeroModifier`. Needs a modifier registry.
pub const CW235_ZERO_MODIFIER: ErrorCode = ErrorCode {
    id: "CW235",
    severity: ErrorSeverity::Warning,
    message_template: "Modifier {} has value 0. Modifiers are additive so likely doesn't do anything",
};

/// A list could be merged for optimisation. F# `OptimisationMergeList`.
pub const CW269_OPTIMISATION_MERGE_LIST: ErrorCode = ErrorCode {
    id: "CW269",
    severity: ErrorSeverity::Hint,
    message_template: "Optimise by merging this with {} by using {}",
};

/// A planet_killer is missing required config. F# `PlanetKillerMissing`.
pub const CW250_PLANET_KILLER_MISSING: ErrorCode = ErrorCode {
    id: "CW250",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

// CW400 (unknown scope reference) retired: converged onto F#'s CW247
// (ConfigRulesRuleWrongScope). See lib.rs scope checks.

/// Type instance not found. Rust-only extension (no F# equivalent): an
/// `<type>` reference that resolves to no known instance. Distinct from the
/// event-specific CW222.
pub const CW500_TYPE_NOT_FOUND: ErrorCode = ErrorCode {
    id: "CW500",
    severity: ErrorSeverity::Error,
    message_template: "Type '{}' not found",
};

// CW501 (duplicate type) retired: converged onto F#'s CW261
// (`CW261_DUPLICATE_TYPE_DEF`). See the reconciliation note there.

// CW502 (unused type) retired: converged onto F#'s CW239 (`UnusedType`) below.
// See the reconciliation note on CW239.

/// A `should_be_referenced` type instance that is never referenced anywhere.
/// F# `UnusedType`. Reconciliation: cwtools-rs reserved the Rust-invented CW502
/// for this; F#'s ID is CW239. Converging on CW239. Args are `(referenceName, typeName)`.
/// Emitted by the batch path only (`validation::references`): the answer needs
/// every file's references at once.
pub const CW239_UNUSED_TYPE: ErrorCode = ErrorCode {
    id: "CW239",
    severity: ErrorSeverity::Warning,
    message_template: "{} of type {} is not used anywhere, but is expected to be",
};

// ── Rules-config diagnostics (Rust-only) ───────────────
//
// Problems in the `.cwt` ruleset itself rather than in the script it checks. F#
// only ever printed these as text, so there is no ID to converge on. They are
// emitted by `cwtools_rules` and reported against the `.cwt` file that carries
// them, so a broken ruleset fails CI instead of degrading every later check in
// silence.

/// A `.cwt` file or rules directory the loader could not read: a missing or
/// unreadable path, or a file over the scan budget. The offending path is the
/// diagnostic's file.
pub const CW600_RULES_FILE_UNREADABLE: ErrorCode = ErrorCode {
    id: "CW600",
    severity: ErrorSeverity::Error,
    message_template: "Rules file could not be read: {}",
};

/// A rule naming a type, enum or single_alias that no `.cwt` file defines.
/// Args are `(kind, name)`.
pub const CW601_RULES_UNDEFINED_REFERENCE: ErrorCode = ErrorCode {
    id: "CW601",
    severity: ErrorSeverity::Error,
    message_template: "Rule references undefined {} `{}`",
};

/// A `single_alias_right[...]` the post-processor refused to expand: a
/// reference cycle, a chain past the depth limit, or the node budget. Reported
/// on the `single_alias` definition it names. The message is built at the emit
/// site.
pub const CW602_RULES_UNEXPANDED_ALIAS: ErrorCode = ErrorCode {
    id: "CW602",
    severity: ErrorSeverity::Error,
    message_template: "{}",
};

/// A `##` directive whose value the loader can't parse, so the option silently
/// falls back to its default. The message is built at the emit site.
pub const CW603_RULES_INVALID_DIRECTIVE: ErrorCode = ErrorCode {
    id: "CW603",
    severity: ErrorSeverity::Warning,
    message_template: "{}",
};

// error_code_hash deleted: no callers, and it wasn't actually a hash.

// ── The catalog as a list ──────────────────────────────
//
// The consts above are the definitions; this is the one enumeration of them,
// paired with the const name so a caller can derive a rule name from it. The
// CLI validates `--ignore-code`/`--only-code` against it and builds its SARIF
// rule metadata from it, and `mod translations` checks its tables against it.
// `catalog_lists_every_code` in the CLI fails when a new `pub const … :
// ErrorCode` here is not added below.

macro_rules! catalog {
    ($($name:ident),+ $(,)?) => {
        /// Known diagnostic codes, including wired and pending checks, with the
        /// const each is declared as.
        pub const CATALOG: &[(&str, ErrorCode)] = &[
            $((stringify!($name), $name),)+
        ];
    };
}

catalog![
    CW001_PARSE_ERROR,
    CW100_MISSING_LOCALISATION,
    CW104_INCORRECT_TRIGGER_SCOPE,
    CW105_INCORRECT_EFFECT_SCOPE,
    CW106_INCORRECT_SCOPE_SCOPE,
    CW107_EVENT_EVERY_TICK,
    CW108_RESEARCH_LEADER_AREA,
    CW109_RESEARCH_LEADER_TECH,
    CW110_TECH_CAT_MISSING,
    CW113_MISSING_FILE,
    CW120_POSSIBLE_PRETRIGGER,
    CW121_EMPTY_IF,
    CW122_LOC_KEY_IN_INLINE,
    CW220_UNSAVED_EVENT_TARGET,
    CW221_MAYBE_UNSAVED_EVENT_TARGET,
    CW222_UNDEFINED_EVENT,
    CW223_INCORRECT_NOT_USAGE,
    CW225_UNDEFINED_LOC_REFERENCE,
    CW226_INVALID_LOC_COMMAND,
    CW227_UNKNOWN_SECTION_TEMPLATE,
    CW228_MISSING_SECTION_SLOT,
    CW229_UNKNOWN_COMPONENT_TEMPLATE,
    CW230_MISMATCHED_COMPONENT_AND_SLOT,
    CW231_UNUSED_TECH,
    CW233_UNDEFINED_ENTITY,
    CW234_REPLACE_ME_LOC,
    CW235_ZERO_MODIFIER,
    CW236_DEPRECATED_ELSE,
    CW237_AMBIGUOUS_IF_ELSE,
    CW238_IF_ELSE_ORDER,
    CW239_UNUSED_TYPE,
    CW240_UNEXPECTED_VALUE,
    CW242_WRONG_NUMBER,
    CW243_TARGET_WRONG_SCOPE,
    CW244_INVALID_TARGET,
    CW245_ERROR_IN_TARGET,
    CW246_UNSET_VARIABLE,
    CW247_RULE_WRONG_SCOPE,
    CW248_INVALID_SCOPE_COMMAND,
    CW250_PLANET_KILLER_MISSING,
    CW251_UNNECESSARY_BOOLEAN,
    CW253_DEPRECATED_SET_NAME,
    CW254_WRONG_ENCODING,
    CW255_MISSING_LOC_FILE_LANG,
    CW256_MISSING_LOC_FILE_LANG_HEADER,
    CW257_LOC_FILE_LANG_MISMATCH,
    CW259_RECURSIVE_LOC_REF,
    CW260_LOC_COMMAND_WRONG_SCOPE,
    CW261_DUPLICATE_TYPE_DEF,
    CW262_UNEXPECTED_PROPERTY_NODE,
    CW263_UNEXPECTED_PROPERTY_LEAF,
    CW264_UNEXPECTED_PROPERTY_LEAF_VALUE,
    CW265_UNEXPECTED_PROPERTY_VALUE_CLAUSE,
    CW266_LOC_COMMAND_NOT_IN_DATA_TYPE,
    CW267_UNEXPECTED_ALIAS_KEY_VALUE,
    CW268_LOC_MISSING_QUOTE,
    CW269_OPTIMISATION_MERGE_LIST,
    CW270_VARIABLE_TOO_SMALL,
    CW271_VARIABLE_INT_ONLY,
    CW272_FROM_RULES_CUSTOM_ERROR,
    CW273_UNDEFINED_MODIFIER_TYPE,
    CW274_INLINE_SCRIPT_ERROR,
    CW275_LOC_INVALID_CHARS,
    CW276_LOC_KEY_INVALID_CHARS,
    CW277_ALIAS_BRANCH_LIMIT,
    CW280_REDUNDANT_DEFAULT_FIELD,
    CW281_EMPTY_LIMIT,
    CW282_REDUNDANT_DEFAULT_BOOL,
    CW283_SCRIPTED_GUI_CALLBACK_NOT_FOUND,
    CW500_TYPE_NOT_FOUND,
    CW600_RULES_FILE_UNREADABLE,
    CW601_RULES_UNDEFINED_REFERENCE,
    CW602_RULES_UNEXPANDED_ALIAS,
    CW603_RULES_INVALID_DIRECTIVE,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The list above is hand-written, so a code declared without a line in it
    /// would be invisible to `--ignore-code` and to the translation tables.
    /// Diff the two straight from the source rather than trusting the list.
    #[test]
    fn catalog_covers_supported_error_code_consts() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("this crate's own source");
        const RETIRED_CODES: &[&str] = &["CW258_LOC_FILE_LANG_WRONG_PLACE"];

        let mut declared: Vec<&str> = src
            .lines()
            .filter_map(|l| {
                let (name, ty) = l.strip_prefix("pub const ")?.split_once(':')?;
                ty.trim_start().starts_with("ErrorCode").then_some(name)
            })
            .filter(|name| !RETIRED_CODES.contains(name))
            .collect();
        declared.sort_unstable();
        let mut listed: Vec<&str> = CATALOG.iter().map(|(name, _)| *name).collect();
        listed.sort_unstable();
        assert_eq!(
            declared, listed,
            "CATALOG must list every supported ErrorCode const in this file"
        );
    }

    #[test]
    fn format_substitutes_positionally() {
        assert_eq!(
            CW100_MISSING_LOCALISATION.format(&["my_key", "english"]),
            "Localisation key my_key is not defined for english"
        );
    }

    #[test]
    fn the_default_locale_answers_in_english() {
        assert_eq!(
            CW271_VARIABLE_INT_ONLY.message(),
            CW271_VARIABLE_INT_ONLY.message_template
        );
        assert_eq!(cw223_hoi4_message(), CW223_INCORRECT_NOT_USAGE_HOI4_MSG);
    }
}
