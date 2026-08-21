use super::common::{as_block, child_key_eq, key_len, under_dir_segment, walk_blocks};
use crate::common::value_is_zero;
use crate::ctx::ValidationCtx;
use crate::{ValidationError, error_codes};
use cwtools_index::TypeIndex;
use cwtools_parser::ast::{Child, ParsedFile, Value};
use cwtools_parser::fix::{SuggestedFix, key_token_range};
use cwtools_rules::rules_types::RuleSet;
use cwtools_string_table::string_table::{StringId, StringTable};

/// The block keys these checks dispatch on, interned once per file so the walks
/// compare token ids instead of pulling every block's key out of the string
/// table. Paradox keys are case-insensitive, so each is a `lower` id.
struct Keys {
    set_empire_name: StringId,
    set_planet_name: StringId,
    if_: StringId,
    else_: StringId,
    else_if: StringId,
    ship_design: StringId,
    global_ship_design: StringId,
    section: StringId,
    component: StringId,
    research_leader: StringId,
}

impl Keys {
    fn new(table: &StringTable) -> Self {
        Self {
            set_empire_name: table.intern("set_empire_name").lower,
            set_planet_name: table.intern("set_planet_name").lower,
            if_: table.intern("if").lower,
            else_: table.intern("else").lower,
            else_if: table.intern("else_if").lower,
            ship_design: table.intern("ship_design").lower,
            global_ship_design: table.intern("global_ship_design").lower,
            section: table.intern("section").lower,
            component: table.intern("component").lower,
            research_leader: table.intern("research_leader").lower,
        }
    }
}

/// Stellaris-specific validators.
/// Ported from CWTools/Validation/Stellaris/STLValidation.fs
pub fn validate_stellaris(
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    file_path: &crate::FilePath,
    type_index: Option<&TypeIndex>,
    errors: &mut Vec<ValidationError>,
) {
    // F# scopes each validator to its entity type (folder), not key names alone.
    let in_events = under_dir_segment(file_path, "events");
    let in_technology = parent_dir_is(file_path, "common/technology");
    let in_pop_jobs = parent_dir_is(file_path, "common/pop_jobs");
    let in_component_templates = parent_dir_is(file_path, "common/component_templates");
    let kw = Keys::new(table);

    for child in &ast.root_children {
        let Some(block) = as_block(child, ast) else {
            continue;
        };
        // The event keys are an open set (`planet_event`, `country_event`, …), so
        // this one match needs the text — but only inside an events folder, and
        // only for a block that actually is one.
        if in_events
            && table
                .with_string(block.key_lower, |k| k.ends_with("_event") || k == "event")
                .unwrap_or(false)
        {
            let key = block.key_string_lower(table);
            validate_event_pretriggers(
                &key,
                block.children,
                ast,
                ruleset,
                table,
                file_path,
                errors,
            );
        }
        if in_technology {
            validate_technology(
                block.children,
                block.range.start.line,
                ast,
                table,
                &kw,
                file_path,
                errors,
            );
        }
        if in_pop_jobs {
            validate_pop_job(block.children, ast, ruleset, table, file_path, errors);
        }
        if in_component_templates {
            validate_planet_killer(
                block.children,
                block.range.start.line,
                ast,
                table,
                file_path,
                type_index,
                errors,
            );
        }
    }

    // CW227 / CW229: walk ship_design blocks at root for section/component
    // template lookups. Skipped when no type index is loaded.
    validate_ship_designs(
        &ast.root_children,
        ast,
        table,
        &kw,
        file_path,
        type_index,
        errors,
    );

    // Stellaris-specific structural hints (if/else 2.1, deprecated set_name).
    walk_if_else(&ast.root_children, ast, table, &kw, file_path, errors);
}

// ── Path scoping helpers ───────────────────────────────

/// Directory part of `file_path`, `/`-normalised and lowercased.
fn dir_of(file_path: &str) -> Option<String> {
    let norm = file_path.replace('\\', "/");
    norm.rsplit_once('/')
        .map(|(dir, _)| dir.to_ascii_lowercase())
}

/// True when the file sits directly in `suffix`, not a subfolder of it
/// (`common/technology/category/` holds categories, not technologies).
fn parent_dir_is(file_path: &str, suffix: &str) -> bool {
    dir_of(file_path).is_some_and(|dir| dir == suffix || dir.ends_with(&format!("/{suffix}")))
}

// ── If/Else & set_name structural hints (CW236/CW237/CW253) ─────────

fn walk_if_else(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keys,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    walk_blocks(children, ast, &mut |block| {
        let key = block.key_lower;
        let block_children = block.children;
        let line = block.range.start.line;
        let col = block.range.start.col;

        // CW253 — deprecated set_empire_name / set_planet_name.
        if key == kw.set_empire_name || key == kw.set_planet_name {
            // Fix: rename just the key token to `set_name` (the block body is
            // unchanged). The squiggle covers the same span, since the body is
            // not what's deprecated.
            let key_range = key_token_range(block.range.start, key_len(table, key));
            let fix = SuggestedFix::replace(
                cwtools_i18n::t(cwtools_i18n::Key::ActionRenameToSetName),
                key_range,
                "set_name",
            );
            errors.push(
                ValidationError::from_code(
                    &error_codes::CW253_DEPRECATED_SET_NAME,
                    file_path,
                    line,
                    col,
                    &[],
                )
                .with_fix(fix)
                .with_end(key_range.end),
            );
        }

        if key == kw.if_ || key == kw.else_if {
            let has_else = block_children
                .iter()
                .any(|c| child_key_eq(c, ast, table, "else"));
            let has_if = block_children
                .iter()
                .any(|c| child_key_eq(c, ast, table, "if"));
            // Advice about the keyword, so the squiggle covers the key, not the
            // whole block it opens (same treatment CW253 got above).
            let key_end = key_token_range(block.range.start, key_len(table, key)).end;

            // CW236 — old nested if/else style.
            if has_else && !has_if {
                errors.push(
                    ValidationError::from_code(
                        &error_codes::CW236_DEPRECATED_ELSE,
                        file_path,
                        line,
                        col,
                        &[],
                    )
                    .with_end(key_end),
                );
            }

            // CW237 — ambiguous if = { if ... else }.
            if key == kw.if_ && has_else && has_if {
                // 2.1 changed which `if` the nested `else` binds to, so the
                // `else` is the branch to go and read. A non-block `else` has
                // no span to point at, and the diagnostic keeps just its own.
                let else_range = block_children
                    .iter()
                    .filter_map(|c| as_block(c, ast))
                    .find(|b| b.key_lower == kw.else_)
                    .map(|b| key_token_range(b.range.start, key_len(table, b.key_lower)));
                let mut err = ValidationError::from_code(
                    &error_codes::CW237_AMBIGUOUS_IF_ELSE,
                    file_path,
                    line,
                    col,
                    &[],
                )
                .with_end(key_end);
                if let Some(range) = else_range {
                    err = err.with_related("this else changed which if it binds to", range);
                }
                errors.push(err);
            }
        }
    });
}

// ── Event Validation (CW120) ───────────────────────────

/// Pretrigger set for an event key (`planet_event` -> `planet_pre_trigger` names).
/// `pop_group_event` maps to the `pop` category; event types with no
/// `<scope>_pre_trigger` category in the config return None.
fn event_pretriggers<'a>(
    event_key: &str,
    ruleset: &'a RuleSet,
) -> Option<&'a rustc_hash::FxHashSet<String>> {
    let scope = event_key.strip_suffix("_event")?;
    let scope = if scope == "pop_group" { "pop" } else { scope };
    ruleset.pretriggers().get(scope)
}

/// Validate a Stellaris event body for pretrigger placement (CW120).
fn validate_event_pretriggers(
    event_key: &str,
    children: &[Child],
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    // CW120: a trigger-block pretrigger could move to `pre_triggers` for perf.
    // F#'s validatePreTriggers was hardcoded to planet_event; this uses the
    // config's per-scope sets instead.
    let Some(pretriggers) = event_pretriggers(event_key, ruleset) else {
        return;
    };
    for child in children {
        if !child_key_eq(child, ast, table, "trigger") {
            continue;
        }
        let Some(trigger) = as_block(child, ast) else {
            continue;
        };
        flag_pretriggers(trigger.children, ast, pretriggers, table, file_path, errors);
    }
}

/// Emit CW120 for every direct leaf of `children` naming a known pretrigger.
/// Direct leaves only: F# doesn't recurse into `limit`/`AND`/... sub-blocks,
/// since a nested condition can't be lifted wholesale.
fn flag_pretriggers(
    children: &[Child],
    ast: &ParsedFile,
    pretriggers: &rustc_hash::FxHashSet<String>,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    for tc in children {
        let Child::Leaf(idx) = tc else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        if matches!(leaf.value, Value::Clause(_)) {
            continue;
        }
        let leaf_key = table.get_string(leaf.key.lower).unwrap_or_default();
        if pretriggers.contains(&leaf_key) {
            let code = &error_codes::CW120_POSSIBLE_PRETRIGGER;
            errors.push(
                ValidationError::from_code(
                    code,
                    file_path,
                    leaf.pos.start.line,
                    leaf.pos.start.col,
                    &[&leaf_key],
                )
                .with_end(leaf.pos.end),
            );
        }
    }
}

// ── Pop Jobs (CW120) ───────────────────────────────────
//
// A pop pretrigger in a job's `possible` block could move to `possible_pre_triggers`.
fn validate_pop_job(
    children: &[Child],
    ast: &ParsedFile,
    ruleset: &RuleSet,
    table: &StringTable,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    let Some(pretriggers) = ruleset.pretriggers().get("pop") else {
        return;
    };
    for child in children {
        if !child_key_eq(child, ast, table, "possible") {
            continue;
        }
        let Some(possible) = as_block(child, ast) else {
            continue;
        };
        flag_pretriggers(
            possible.children,
            ast,
            pretriggers,
            table,
            file_path,
            errors,
        );
    }
}

// ── Ship Design Validation (CW227 / CW229) ───────────────────────────────
//
// Gated like CW500: only runs when the type index is complete and has known
// instances of the looked-up type, so a mod validated without vanilla doesn't
// flag every vanilla template reference.
fn validate_ship_designs(
    root_children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keys,
    file_path: &crate::FilePath,
    type_index: Option<&TypeIndex>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(type_index) = type_index else {
        return;
    };
    if !type_index.complete {
        return;
    }
    // Engine-builtin sections that no file defines (F#'s defaultTemplates).
    const DEFAULT_SECTION_TEMPLATES: &[&str] = &[
        "DEFAULT_COLONIZATION_SECTION",
        "DEFAULT_CONSTRUCTION_SECTION",
    ];
    for child in root_children {
        let Some(block) = as_block(child, ast) else {
            continue;
        };
        if block.key_lower != kw.ship_design && block.key_lower != kw.global_ship_design {
            continue;
        }
        for grandchild in block.children {
            let Some(gc_block) = as_block(grandchild, ast) else {
                continue;
            };
            let gc_key = gc_block.key_lower;
            let (type_name, code) = if gc_key == kw.section {
                (
                    "section_template",
                    &error_codes::CW227_UNKNOWN_SECTION_TEMPLATE,
                )
            } else if gc_key == kw.component {
                (
                    "component_template",
                    &error_codes::CW229_UNKNOWN_COMPONENT_TEMPLATE,
                )
            } else {
                continue;
            };
            let Some(template) = child_scalar(gc_block.children, ast, table, "template") else {
                continue;
            };
            if type_name == "section_template"
                && DEFAULT_SECTION_TEMPLATES
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(&template))
            {
                continue;
            }
            if type_index.instances(type_name).is_empty()
                || type_index.contains(type_name, &template)
            {
                continue;
            }
            errors.push(
                ValidationError::from_code(
                    code,
                    file_path,
                    gc_block.range.start.line,
                    gc_block.range.start.col,
                    &[&template],
                )
                .with_end(key_token_range(gc_block.range.start, key_len(table, gc_key)).end),
            );
        }
    }
}

// ── Technology (CW108 / CW109 / CW110) ─────────────────
//
// Every root block of a `common/technology/*.txt` file is a technology.
// CW110: the game refuses to load a tech with no `category` value.
fn validate_technology(
    children: &[Child],
    tech_line: u32,
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keys,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    if !technology_has_category(children, ast, table) {
        errors.push(ValidationError::from_code(
            &error_codes::CW110_TECH_CAT_MISSING,
            file_path,
            tech_line,
            0,
            &[],
        ));
    }

    let tech_area = child_scalar(children, ast, table, "area").unwrap_or_default();
    walk_research_leaders(children, &tech_area, ast, table, kw, file_path, errors);
}

/// `category = { physics }` (block with a member, the game's form) or a bare
/// `category = physics` scalar both count; an empty `category = { }` doesn't.
fn technology_has_category(children: &[Child], ast: &ParsedFile, table: &StringTable) -> bool {
    for c in children {
        let Child::Leaf(idx) = c else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        let is_cat = table
            .with_string(leaf.key.normal, |k| k.eq_ignore_ascii_case("category"))
            .unwrap_or(false);
        if !is_cat {
            continue;
        }
        let has_value = match &leaf.value {
            Value::Clause(cs) => cs.iter().any(|m| matches!(m, Child::LeafValue(_))),
            Value::String(t) | Value::QString(t) => table
                .with_string(t.normal, |s| !s.is_empty())
                .unwrap_or(false),
            _ => false,
        };
        if has_value {
            return true;
        }
    }
    false
}

/// Recursively find `research_leader` blocks under a technology (they sit
/// inside `weight_modifier`, never at file root).
fn walk_research_leaders(
    children: &[Child],
    tech_area: &str,
    ast: &ParsedFile,
    table: &StringTable,
    kw: &Keys,
    file_path: &crate::FilePath,
    errors: &mut Vec<ValidationError>,
) {
    walk_blocks(children, ast, &mut |block| {
        if block.key_lower == kw.research_leader {
            let key_end = key_token_range(block.range.start, key_len(table, block.key_lower)).end;
            match child_scalar(block.children, ast, table, "area") {
                None => {
                    let code = &error_codes::CW108_RESEARCH_LEADER_AREA;
                    errors.push(
                        ValidationError::from_code(
                            code,
                            file_path,
                            block.range.start.line,
                            block.range.start.col,
                            &[],
                        )
                        .with_end(key_end),
                    );
                }
                Some(leader_area)
                    if !tech_area.is_empty() && !leader_area.eq_ignore_ascii_case(tech_area) =>
                {
                    // F# swapped these args (tech first); ours is leader-then-tech.
                    let code = &error_codes::CW109_RESEARCH_LEADER_TECH;
                    errors.push(
                        ValidationError::from_code(
                            code,
                            file_path,
                            block.range.start.line,
                            block.range.start.col,
                            &[&leader_area, tech_area],
                        )
                        .with_end(key_end),
                    );
                }
                _ => {}
            }
        }
    });
}

// ── Planet Killer (CW250) ──────────────────────────────
//
// A component template with `type = planet_killer` needs a matching
// `on_destroy_planet_with_<key>` on_action and `can_destroy_planet_with_<key>`
// scripted trigger, or the weapon can't fire.
fn validate_planet_killer(
    children: &[Child],
    block_line: u32,
    ast: &ParsedFile,
    table: &StringTable,
    file_path: &crate::FilePath,
    type_index: Option<&TypeIndex>,
    errors: &mut Vec<ValidationError>,
) {
    let Some(type_index) = type_index else {
        return;
    };
    if !type_index.complete {
        return;
    }
    let is_planet_killer = child_scalar(children, ast, table, "type")
        .is_some_and(|t| t.eq_ignore_ascii_case("planet_killer"));
    if !is_planet_killer {
        return;
    }
    let Some(key) = child_scalar(children, ast, table, "key") else {
        return;
    };

    let code = &error_codes::CW250_PLANET_KILLER_MISSING;
    let checks: &[(&str, &str, &str)] = &[
        ("on_action", "on_action", "on_destroy_planet_with_"),
        (
            "scripted_trigger",
            "scripted trigger",
            "can_destroy_planet_with_",
        ),
    ];
    for (type_name, label, prefix) in checks {
        // Same completeness rule as CW500: only flag when we actually know
        // instances of the looked-up type.
        if type_index.instances(type_name).is_empty() {
            continue;
        }
        let wanted = format!("{prefix}{key}");
        if !type_index.contains(type_name, &wanted) {
            errors.push(ValidationError::from_code(
                code,
                file_path,
                block_line,
                0,
                &[&format!("Planet killer {key} is missing {label} {wanted}")],
            ));
        }
    }
}

// ── Unused technology (CW231) ──────────────────────────
//
// Whether a technology is used is a project-wide question: the answer lives in
// the `<technology>` references the rule engine collects across every file (a
// prerequisite of another tech, a building's unlock, a `has_technology` check).
// See `crate::references`. This half only handles the technologies the game uses
// without any script naming them, which F# `validateTechnologies` exempts before
// reporting `UnusedTech`: marking them used keeps CW231 quiet without inventing
// a second notion of "used".

/// Mark the technologies in this file that the game uses on its own: one that
/// grants a modifier, documents what it unlocks, is weighted out of the draw, or
/// sits behind a feature flag.
pub(super) fn mark_exempt_technologies(ctx: &ValidationCtx) {
    if !ctx.tracks_type_uses(crate::references::TECHNOLOGY)
        || !parent_dir_is(ctx.file_path, "common/technology")
    {
        return;
    }
    for child in &ctx.ast.root_children {
        let Some(block) = as_block(child, ctx.ast) else {
            continue;
        };
        if technology_is_exempt(block.children, ctx.ast, ctx.table) {
            let key = block.key_string_lower(ctx.table);
            ctx.mark_type_use(crate::references::TECHNOLOGY, &key);
        }
    }
}

fn technology_is_exempt(children: &[Child], ast: &ParsedFile, table: &StringTable) -> bool {
    let has = |key: &str| children.iter().any(|c| child_key_eq(c, ast, table, key));
    if has("prereqfor_desc") || has("modifier") || has("feature_flags") {
        return true;
    }
    // A tech the draw can never pick is unreachable by design, not unused.
    if children.iter().any(|c| {
        let Child::Leaf(idx) = c else { return false };
        let leaf = &ast.arena.leaves[*idx as usize];
        child_key_eq(c, ast, table, "weight") && value_is_zero(&leaf.value)
    }) {
        return true;
    }
    children.iter().any(|c| {
        let Some(block) = as_block(c, ast) else {
            return false;
        };
        table
            .with_string(block.key_lower, |k| k == "weight_modifier")
            .unwrap_or(false)
            && block.children.iter().any(|wc| {
                let Child::Leaf(idx) = wc else { return false };
                let leaf = &ast.arena.leaves[*idx as usize];
                child_key_eq(wc, ast, table, "factor") && value_is_zero(&leaf.value)
            })
    })
}

// ── Helpers ────────────────────────────────────────────

/// Scalar value of the first child leaf whose key matches `key` (case-insensitive),
/// or None if the key is absent or the leaf carries a clause.
fn child_scalar(
    children: &[Child],
    ast: &ParsedFile,
    table: &StringTable,
    key: &str,
) -> Option<String> {
    for c in children {
        let Child::Leaf(idx) = c else { continue };
        let leaf = &ast.arena.leaves[*idx as usize];
        let matches_key = table
            .with_string(leaf.key.normal, |k| k.eq_ignore_ascii_case(key))
            .unwrap_or(false);
        if !matches_key {
            continue;
        }
        return match &leaf.value {
            // QString tokens keep their quotes; `template = "SSM_..."` must
            // yield the bare name.
            Value::String(t) | Value::QString(t) => Some(
                table
                    .get_string(t.normal)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string(),
            ),
            Value::Bool(b) => Some(b.to_string()),
            Value::Int(n) => Some(n.to_string()),
            Value::Float(n) => Some(n.to_string()),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cwtools_index::{SourceLocation, TypeInstance};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_types::{NewField, Options, RuleType, ValueType};

    const EVENTS: &str = "events/test.txt";
    const TECH: &str = "common/technology/test.txt";
    const POP_JOBS: &str = "common/pop_jobs/test.txt";
    const COMPONENTS: &str = "common/component_templates/test.txt";

    fn codes_at(path: &str, script: &str) -> Vec<(String, u32, u16)> {
        codes_with(path, script, &RuleSet::new(), None)
    }

    /// Build a RuleSet's pretrigger map from `(scope, names)` pairs, mirroring
    /// what `reindex()` produces from the config's `alias[<scope>_pre_trigger:<name>]`.
    fn ruleset_with_pretriggers(categories: &[(&str, &[&str])]) -> RuleSet {
        // Build via aliases so reindex populates the derived pretriggers map
        // rather than mutating the private derived index directly.
        let mut rs = RuleSet::new();
        for (scope, names) in categories {
            for name in *names {
                let alias_name = format!("{scope}_pre_trigger:{name}");
                rs.aliases.push((
                    alias_name,
                    (
                        RuleType::LeafRule {
                            left: NewField::SpecificField(name.to_string()),
                            right: NewField::ValueField(ValueType::Bool),
                        },
                        Options::default(),
                    ),
                ));
            }
        }
        rs.reindex();
        rs
    }

    /// A complete TypeIndex holding the given `(type, instance)` pairs.
    fn index_with(entries: &[(&str, &str)]) -> TypeIndex {
        let mut per_type: std::collections::HashMap<String, Vec<TypeInstance>> = Default::default();
        for (ty, name) in entries {
            per_type
                .entry((*ty).to_string())
                .or_default()
                .push(TypeInstance {
                    name: (*name).to_string(),
                    location: SourceLocation {
                        line: 0,
                        col: 0,
                        end: (0, 0),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                });
        }
        let mut idx = TypeIndex::new();
        idx.merge("test://config.txt", per_type);
        idx.complete = true;
        idx
    }

    fn codes_with(
        path: &str,
        script: &str,
        ruleset: &RuleSet,
        type_index: Option<&TypeIndex>,
    ) -> Vec<(String, u32, u16)> {
        let table = StringTable::new();
        let ast = parse_string(script, &table);
        let mut errors = Vec::new();
        validate_stellaris(&ast, ruleset, &table, &path.into(), type_index, &mut errors);
        errors
            .into_iter()
            .filter_map(|e| e.code.map(|c| (c.to_string(), e.line, e.col)))
            .collect()
    }

    fn has_code(codes: &[(String, u32, u16)], code: &str) -> bool {
        codes.iter().any(|(c, _, _)| c == code)
    }

    #[test]
    fn cw253_fix_renames_key_to_set_name() {
        use cwtools_parser::fix::apply_edits;
        let src = "set_empire_name = { key = \"NAME\" }\n";
        let table = StringTable::new();
        let ast = parse_string(src, &table);
        let ruleset = RuleSet::new();
        let mut errors = Vec::new();
        validate_stellaris(
            &ast,
            &ruleset,
            &table,
            &"events/test.txt".into(),
            None,
            &mut errors,
        );

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW253"))
            .expect("CW253 emitted");
        let fix = err.fix.as_ref().expect("CW253 carries a fix");
        let fixed = apply_edits(src, &fix.edits);
        assert_eq!(fixed, "set_name = { key = \"NAME\" }\n");

        // Revalidation of the fixed text no longer emits CW253.
        let ast2 = parse_string(&fixed, &table);
        let mut errors2 = Vec::new();
        validate_stellaris(
            &ast2,
            &ruleset,
            &table,
            &"events/test.txt".into(),
            None,
            &mut errors2,
        );
        assert!(
            !errors2.iter().any(|e| e.code == Some("CW253")),
            "CW253 must be gone after applying the fix"
        );
    }

    fn count_code(codes: &[(String, u32, u16)], code: &str) -> usize {
        codes.iter().filter(|(c, _, _)| c == code).count()
    }

    #[test]
    fn child_key_eq_is_case_insensitive() {
        let table = StringTable::new();
        let ast = parse_string("root = {\n IF = {}\n Trigger = {}\n}\n", &table);
        let block = as_block(&ast.root_children[0], &ast).expect("root is a block");
        assert!(
            block
                .children
                .iter()
                .any(|c| child_key_eq(c, &ast, &table, "if")),
            "`IF` should match expected `if`"
        );
        assert!(
            block
                .children
                .iter()
                .any(|c| child_key_eq(c, &ast, &table, "trigger")),
            "`Trigger` should match expected `trigger`"
        );
    }

    // ── Pre-trigger placement (CW120, event-scoped) ───────────────────────────

    #[test]
    fn pre_trigger_inside_trigger_is_cw120() {
        let rs = ruleset_with_pretriggers(&[("planet", &["is_ai"])]);
        let c = codes_with(
            EVENTS,
            "planet_event = {\n\
             mean_time_to_happen = { years = 5 }\n\
             trigger = { is_ai = yes }\n\
             }\n",
            &rs,
            None,
        );
        assert!(
            has_code(&c, "CW120"),
            "pre-trigger inside trigger block should emit CW120, got: {:?}",
            c
        );
        assert_eq!(
            count_code(&c, "CW120"),
            1,
            "exactly one diagnostic per pretrigger leaf, got: {:?}",
            c
        );
    }

    #[test]
    fn pre_trigger_at_root_is_clean() {
        let rs = ruleset_with_pretriggers(&[("planet", &["is_ai"])]);
        let c = codes_with(
            EVENTS,
            "planet_event = {\n\
             mean_time_to_happen = { years = 5 }\n\
             is_ai = yes\n\
             }\n",
            &rs,
            None,
        );
        assert!(!has_code(&c, "CW120"), "got: {:?}", c);
    }

    #[test]
    fn pre_trigger_wrong_scope_event_is_clean() {
        // fleet_event has no pretrigger category at all, so nothing fires here.
        let rs = ruleset_with_pretriggers(&[("planet", &["is_ai"])]);
        let c = codes_with(
            EVENTS,
            "fleet_event = {\n\
             mean_time_to_happen = { years = 5 }\n\
             trigger = { is_ai = yes }\n\
             }\n",
            &rs,
            None,
        );
        assert!(
            !has_code(&c, "CW120"),
            "fleet_event has no pretrigger category, got: {:?}",
            c
        );
    }

    #[test]
    fn pre_trigger_scope_sets_are_separate() {
        // `is_enslaved` is pop-only: quiet for planet_event, flagged for pop_event.
        let rs = ruleset_with_pretriggers(&[("pop", &["is_enslaved"]), ("planet", &["is_ai"])]);
        let planet = codes_with(
            EVENTS,
            "planet_event = { is_triggered_only = yes trigger = { is_enslaved = yes } }\n",
            &rs,
            None,
        );
        assert!(!has_code(&planet, "CW120"), "got: {:?}", planet);
        let pop = codes_with(
            EVENTS,
            "pop_event = { is_triggered_only = yes trigger = { is_enslaved = yes } }\n",
            &rs,
            None,
        );
        assert!(has_code(&pop, "CW120"), "got: {:?}", pop);
    }

    #[test]
    fn pop_group_event_uses_pop_pretriggers() {
        let rs = ruleset_with_pretriggers(&[("pop", &["is_being_purged"])]);
        let c = codes_with(
            EVENTS,
            "pop_group_event = { is_triggered_only = yes trigger = { is_being_purged = yes } }\n",
            &rs,
            None,
        );
        assert!(has_code(&c, "CW120"), "got: {:?}", c);
    }

    #[test]
    fn empty_pretrigger_map_emits_no_cw120() {
        // Config-driven: a name from the old hardcoded list doesn't fire without config backing.
        let c = codes_at(
            EVENTS,
            "planet_event = {\n\
             mean_time_to_happen = { years = 5 }\n\
             trigger = { is_ai = yes }\n\
             }\n",
        );
        assert!(!has_code(&c, "CW120"), "got: {:?}", c);
    }

    #[test]
    fn pre_trigger_nested_in_limit_does_not_fire() {
        let rs = ruleset_with_pretriggers(&[("planet", &["is_ai"])]);
        let c = codes_with(
            EVENTS,
            "planet_event = { is_triggered_only = yes trigger = { limit = { is_ai = yes } } }\n",
            &rs,
            None,
        );
        assert!(!has_code(&c, "CW120"), "got: {:?}", c);
    }

    // ── Pre-trigger placement (CW120, pop jobs) ───────────────────────────────

    #[test]
    fn pop_job_possible_pretrigger_is_cw120() {
        let rs = ruleset_with_pretriggers(&[("pop", &["is_enslaved"])]);
        let c = codes_with(
            POP_JOBS,
            "my_job = { possible = { is_enslaved = yes } }\n",
            &rs,
            None,
        );
        assert!(
            has_code(&c, "CW120"),
            "pop pretrigger inside a job's possible block should emit CW120, got: {:?}",
            c
        );
    }

    #[test]
    fn pop_job_check_only_runs_in_pop_jobs_dir() {
        let rs = ruleset_with_pretriggers(&[("pop", &["is_enslaved"])]);
        let c = codes_with(
            "common/edicts/test.txt",
            "my_edict = { possible = { is_enslaved = yes } }\n",
            &rs,
            None,
        );
        assert!(!has_code(&c, "CW120"), "got: {:?}", c);
    }

    // ── Deprecated set_name (CW253) ───────────────────────────────────────────

    #[test]
    fn set_empire_name_is_cw253() {
        let c = codes_at(EVENTS, "foo = { set_empire_name = { key = \"X\" } }\n");
        assert!(has_code(&c, "CW253"), "got: {:?}", c);
    }

    #[test]
    fn set_planet_name_is_cw253() {
        let c = codes_at(EVENTS, "foo = { set_planet_name = { key = \"Y\" } }\n");
        assert!(has_code(&c, "CW253"), "got: {:?}", c);
    }

    // ── If/else structural hints (CW236/CW237) ────────────────────────────────

    #[test]
    fn deprecated_nested_else_is_cw236() {
        // Old Stellaris style: if = { else = { ... } } without an inner if.
        let c = codes_at(EVENTS, "foo = { if = { limit = { } else = { a = 1 } } }\n");
        assert!(has_code(&c, "CW236"), "got: {:?}", c);
    }

    #[test]
    fn ambiguous_if_with_else_and_inner_if_is_cw237() {
        // `if = { if ... else }` is ambiguous nesting.
        let c = codes_at(
            EVENTS,
            "foo = { if = { limit = { } if = { a = 1 } else = { b = 2 } } }\n",
        );
        assert!(has_code(&c, "CW237"), "got: {:?}", c);
    }

    #[test]
    fn cw237_relates_the_nested_else() {
        let table = StringTable::new();
        let ast = parse_string(
            "foo = {\n    if = {\n        if = { a = 1 }\n        else = { b = 2 }\n    }\n}\n",
            &table,
        );
        let mut errors = Vec::new();
        validate_stellaris(
            &ast,
            &RuleSet::new(),
            &table,
            &EVENTS.into(),
            None,
            &mut errors,
        );

        let err = errors
            .iter()
            .find(|e| e.code == Some("CW237"))
            .expect("CW237 emitted");

        let related = &err.related;
        assert_eq!(related.len(), 1, "got: {related:?}");
        assert_eq!((related[0].line, related[0].col), (4, 8));
        assert_eq!(related[0].end, (4, 8 + "else".len() as u16));
    }

    // ── Technology (CW110) ────────────────────────────────────────────────

    #[test]
    fn technology_without_category_is_cw110() {
        let c = codes_at(TECH, "tech_my_thing = { cost = 100 }\n");
        assert!(
            has_code(&c, "CW110"),
            "tech without category should emit CW110, got: {:?}",
            c
        );
    }

    #[test]
    fn technology_with_category_block_is_clean() {
        let c = codes_at(
            TECH,
            "tech_my_thing = { cost = 100 category = { physics } }\n",
        );
        assert!(!has_code(&c, "CW110"), "got: {:?}", c);
    }

    #[test]
    fn technology_with_scalar_category_is_clean() {
        let c = codes_at(TECH, "tech_my_thing = { cost = 100 Category = physics }\n");
        assert!(!has_code(&c, "CW110"), "got: {:?}", c);
    }

    #[test]
    fn technology_with_empty_category_is_cw110() {
        let c = codes_at(TECH, "tech_my_thing = { cost = 100 category = { } }\n");
        assert!(has_code(&c, "CW110"), "got: {:?}", c);
    }

    #[test]
    fn any_root_key_in_technology_dir_is_a_tech() {
        // tech_ is convention, not a requirement; every root node counts.
        let c = codes_at(TECH, "oddly_named_tech = { cost = 100 }\n");
        assert!(has_code(&c, "CW110"), "got: {:?}", c);
    }

    #[test]
    fn technology_check_skips_category_subdir() {
        // common/technology/category/*.txt holds category definitions.
        let c = codes_at(
            "common/technology/category/test.txt",
            "physics = { led_by = \"x\" }\n",
        );
        assert!(!has_code(&c, "CW110"), "got: {:?}", c);
    }

    #[test]
    fn tech_key_outside_technology_dir_is_not_checked() {
        let c = codes_at("common/scripted_effects/test.txt", "tech_thing = { }\n");
        assert!(!has_code(&c, "CW110"), "got: {:?}", c);
    }

    // ── Research Leader (CW108 / CW109) ───────────────────────────────────

    #[test]
    fn nested_research_leader_without_area_is_cw108() {
        let c = codes_at(
            TECH,
            "tech_my_thing = {\n\
             area = physics\n\
             category = { physics }\n\
             weight_modifier = { research_leader = { modifier = { factor = 2 } } }\n\
             }\n",
        );
        assert!(
            has_code(&c, "CW108"),
            "nested research_leader without area should emit CW108, got: {:?}",
            c
        );
    }

    #[test]
    fn nested_research_leader_with_matching_area_is_clean() {
        let c = codes_at(
            TECH,
            "tech_my_thing = {\n\
             area = physics\n\
             category = { physics }\n\
             weight_modifier = { research_leader = { area = physics } }\n\
             }\n",
        );
        assert!(
            !has_code(&c, "CW108") && !has_code(&c, "CW109"),
            "got: {:?}",
            c
        );
    }

    #[test]
    fn research_leader_area_mismatch_is_cw109() {
        let c = codes_at(
            TECH,
            "tech_my_thing = {\n\
             area = physics\n\
             category = { physics }\n\
             weight_modifier = { research_leader = { area = society } }\n\
             }\n",
        );
        assert!(
            has_code(&c, "CW109"),
            "research_leader area disagreeing with the tech should emit CW109, got: {:?}",
            c
        );
    }

    #[test]
    fn root_research_leader_outside_technology_is_not_checked() {
        // Outside common/technology this is meaningless script; the per-game check stays quiet.
        let c = codes_at(EVENTS, "research_leader = { name = \"Dr. Smith\" }\n");
        assert!(!has_code(&c, "CW108"), "got: {:?}", c);
    }

    // ── Planet Killer (CW250) ──────────────────────────────────────────────

    const PK_TEMPLATE: &str = "weapon_component_template = {\n\
         key = \"PLANET_KILLER_TEST\"\n\
         type = planet_killer\n\
         }\n";

    #[test]
    fn planet_killer_missing_on_action_and_trigger_is_cw250() {
        // Index knows on_actions/triggers, but not the two this one needs: one CW250 each.
        let idx = index_with(&[
            ("on_action", "on_game_start"),
            ("scripted_trigger", "some_trigger"),
        ]);
        let c = codes_with(COMPONENTS, PK_TEMPLATE, &RuleSet::new(), Some(&idx));
        assert_eq!(
            count_code(&c, "CW250"),
            2,
            "missing on_action AND scripted trigger, got: {:?}",
            c
        );
    }

    #[test]
    fn planet_killer_with_both_hooks_is_clean() {
        let idx = index_with(&[
            ("on_action", "on_destroy_planet_with_PLANET_KILLER_TEST"),
            (
                "scripted_trigger",
                "can_destroy_planet_with_PLANET_KILLER_TEST",
            ),
        ]);
        let c = codes_with(COMPONENTS, PK_TEMPLATE, &RuleSet::new(), Some(&idx));
        assert!(!has_code(&c, "CW250"), "got: {:?}", c);
    }

    #[test]
    fn planet_killer_missing_only_trigger_is_one_cw250() {
        let idx = index_with(&[
            ("on_action", "on_destroy_planet_with_PLANET_KILLER_TEST"),
            ("scripted_trigger", "some_other_trigger"),
        ]);
        let c = codes_with(COMPONENTS, PK_TEMPLATE, &RuleSet::new(), Some(&idx));
        assert_eq!(count_code(&c, "CW250"), 1, "got: {:?}", c);
    }

    #[test]
    fn non_planet_killer_component_is_not_checked() {
        let idx = index_with(&[
            ("on_action", "on_game_start"),
            ("scripted_trigger", "some_trigger"),
        ]);
        let c = codes_with(
            COMPONENTS,
            "weapon_component_template = { key = \"GUN\" type = instant }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(!has_code(&c, "CW250"), "got: {:?}", c);
    }

    #[test]
    fn planet_killer_incomplete_index_stays_quiet() {
        // complete = false: the on_action/trigger sets are partial, so no CW250.
        let mut idx = index_with(&[("on_action", "on_game_start")]);
        idx.complete = false;
        let c = codes_with(COMPONENTS, PK_TEMPLATE, &RuleSet::new(), Some(&idx));
        assert!(!has_code(&c, "CW250"), "got: {:?}", c);
    }

    #[test]
    fn planet_killer_outside_component_templates_is_not_checked() {
        let idx = index_with(&[
            ("on_action", "on_game_start"),
            ("scripted_trigger", "some_trigger"),
        ]);
        let c = codes_with(EVENTS, PK_TEMPLATE, &RuleSet::new(), Some(&idx));
        assert!(!has_code(&c, "CW250"), "got: {:?}", c);
    }

    // ── Ship Design (CW227 / CW229) ────────────────────────────────────────

    #[test]
    fn ship_design_section_template_not_found_is_cw227() {
        // Index knows OTHER section templates, so the reference is genuinely unknown.
        let idx = index_with(&[("section_template", "SSM_known_01")]);
        let c = codes_with(
            EVENTS,
            "ship_design = { section = { template = \"SSM_unknown_01\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(
            has_code(&c, "CW227"),
            "unknown section template should emit CW227, got: {:?}",
            c
        );
    }

    #[test]
    fn ship_design_known_section_template_is_clean() {
        let idx = index_with(&[("section_template", "SSM_known_01")]);
        let c = codes_with(
            EVENTS,
            "ship_design = { section = { template = \"SSM_known_01\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(!has_code(&c, "CW227"), "got: {:?}", c);
    }

    #[test]
    fn ship_design_component_template_not_found_is_cw229() {
        let idx = index_with(&[("component_template", "WEAPON_known")]);
        let c = codes_with(
            EVENTS,
            "ship_design = { component = { template = \"WEAPON_unknown\" slot = \"X\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(
            has_code(&c, "CW229"),
            "unknown component template should emit CW229, got: {:?}",
            c
        );
    }

    #[test]
    fn ship_design_default_section_templates_are_exempt() {
        let idx = index_with(&[("section_template", "SSM_known_01")]);
        let c = codes_with(
            EVENTS,
            "ship_design = { section = { template = \"DEFAULT_COLONIZATION_SECTION\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(!has_code(&c, "CW227"), "got: {:?}", c);
    }

    #[test]
    fn ship_design_empty_type_index_stays_quiet() {
        // No section templates indexed at all: can't tell unknown from unindexed, so stay quiet.
        let mut idx = index_with(&[]);
        idx.complete = true;
        let c = codes_with(
            EVENTS,
            "ship_design = { section = { template = \"SSM_known_01\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(!has_code(&c, "CW227"), "got: {:?}", c);
    }

    #[test]
    fn ship_design_incomplete_index_stays_quiet() {
        // Mod validated without vanilla: a vanilla template ref must not false-positive.
        let mut idx = index_with(&[("section_template", "SSM_known_01")]);
        idx.complete = false;
        let c = codes_with(
            EVENTS,
            "ship_design = { section = { template = \"SSM_vanilla_thing\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(!has_code(&c, "CW227"), "got: {:?}", c);
    }

    #[test]
    fn ship_design_walker_skips_non_ship_design_root() {
        // A `foo = { ... }` block at root is not a ship_design, so its inner
        // section/component entries are not validated.
        let idx = index_with(&[("section_template", "SSM_known_01")]);
        let c = codes_with(
            EVENTS,
            "foo = { section = { template = \"X\" slot = \"A\" } }\n",
            &RuleSet::new(),
            Some(&idx),
        );
        assert!(
            !has_code(&c, "CW227") && !has_code(&c, "CW229"),
            "non-ship-design blocks shouldn't fire ship-design validators, got: {:?}",
            c
        );
    }

    #[test]
    fn ship_design_no_type_index_skips_silently() {
        // Passing None for type_index — the validator must not crash and must
        // not emit CW227/CW229 (it has nothing to look up against).
        let c = codes_at(
            EVENTS,
            "ship_design = { section = { template = \"X\" slot = \"A\" } }\n",
        );
        assert!(
            !has_code(&c, "CW227") && !has_code(&c, "CW229"),
            "without a type index, ship-design validators should stay quiet, got: {:?}",
            c
        );
    }
}
