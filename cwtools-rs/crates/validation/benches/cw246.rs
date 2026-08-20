//! CW246's checked-variable-read path (#135).
//!
//! Every `value[variable]` / non-numeric `variable_field` read that reaches
//! `check_variable_get` first asks `is_builtin_variable` (a scan of the
//! config's `value[variable]` builtin list) and `ctx.is_loop_var` before the
//! O(1) `var_index` lookup. This measures `validate_prepared` over a script
//! whose leaves are all checked reads, against a ruleset carrying a
//! HOI4-sized (~480 entries) builtin variable set, a fifth of them declared
//! with an `@<scope>` suffix (mirrors `party_popularity@<ideology>` in the
//! real config).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use cwtools_game::constants::Game;
use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

const BUILTIN_COUNT: usize = 480;
const DEFINED_COUNT: usize = 10;
const LEAVES_PER_KIND: usize = 200;

/// A `.cwt` ruleset declaring `BUILTIN_COUNT` builtin variables (every fifth
/// one carrying an `@<ideology>` family suffix) and a type whose fields cover
/// both CW246-checked shapes: a `value[variable]` get and a non-numeric
/// `variable_field`.
fn rules_source() -> String {
    let mut members = String::new();
    for i in 0..BUILTIN_COUNT {
        if i % 5 == 0 {
            members.push_str(&format!("        builtin_var_{i}@<ideology>\n"));
        } else {
            members.push_str(&format!("        builtin_var_{i}\n"));
        }
    }
    format!(
        r#"
types = {{ type[foo] = {{ path = "game/common/foo" }} }}
values = {{
    value[variable] = {{
{members}    }}
}}
foo = {{
    get = value[variable]
    ref = variable_field
}}
"#
    )
}

/// A script mixing four checked-read shapes: a bare builtin, a builtin read
/// with its own `@`-scope suffix, a project-defined variable, and a genuinely
/// unset one, repeated to mimic a large file.
fn script_source() -> String {
    let mut body = String::new();
    for i in 0..LEAVES_PER_KIND {
        let builtin = (i * 5) % BUILTIN_COUNT;
        let defined = i % DEFINED_COUNT;
        body.push_str(&format!(
            "    get = builtin_var_{builtin}\n\
             get = builtin_var_{builtin}@social_democrat\n\
             ref = defined_var_{defined}\n\
             ref = unset_var_{i}\n"
        ));
    }
    format!("foo = {{\n{body}}}\n")
}

fn bench_cw246(c: &mut Criterion) {
    let table = StringTable::new();
    let rules_ast = parse_string(&rules_source(), &table);
    let ruleset = ast_to_ruleset(&rules_ast, &table);
    let script_ast = parse_string(&script_source(), &table);

    let mut idx = TypeIndex::new();
    for i in 0..DEFINED_COUNT {
        idx.var_index.add_name(&format!("defined_var_{i}"));
    }
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&idx),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: registry.as_ref(),
        scope_checks: true,
        var_checks: true,
    };

    c.bench_function("cw246/checked_variable_reads", |b| {
        b.iter(|| {
            let errors = validate_prepared(
                black_box(&script_ast),
                "game/common/foo/bench.txt",
                black_box(&prepared),
            );
            black_box(errors)
        })
    });
}

criterion_group!(benches, bench_cw246);
criterion_main!(benches);
