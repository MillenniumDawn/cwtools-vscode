//! Allocation regression for scope target probes.
//!
//! A cloned `ScopeContext` allocates a 4-byte `Vec<ScopeId>` for its one-entry
//! scope stack. Compare one target probe with many and require no per-probe
//! allocations of that vector size.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use cwtools_game::constants::Game;
use cwtools_index::TypeIndex;
use cwtools_parser::parser::parse_string;
use cwtools_rules::rules_converter::ast_to_ruleset;
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::{Prepared, build_scope_registry_arc, validate_prepared};

struct AllocationCounter;

static TRACKING: AtomicBool = AtomicBool::new(false);
static SCOPE_ID_SIZED_ALLOCS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

// SAFETY: This allocator delegates every operation unchanged to `System` and
// only observes allocation sizes while a test explicitly enables tracking.
unsafe impl GlobalAlloc for AllocationCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed)
            && layout.size() == size_of::<cwtools_game::scope_engine::ScopeId>()
        {
            SCOPE_ID_SIZED_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded with the original layout to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if TRACKING.load(Ordering::Relaxed)
            && new_size == size_of::<cwtools_game::scope_engine::ScopeId>()
        {
            SCOPE_ID_SIZED_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

const RULES: &str = r#"
scopes = { Country = { aliases = { country } } }
links = { faction_leader = { output_scope = country input_scopes = country } }
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    ## cardinality = 0..inf
    tgt = scope[country]
}
"#;

fn count_scope_sized_allocations(targets: usize) -> usize {
    let table = StringTable::new();
    let ruleset = ast_to_ruleset(&parse_string(RULES, &table), &table);
    let script = format!(
        "foo = {{ {} }}",
        std::iter::repeat_n("tgt = faction_leader", targets)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let parsed = parse_string(&script, &table);
    let index = TypeIndex::new();
    let registry = build_scope_registry_arc(&ruleset, Some(Game::Hoi4));
    let prepared = Prepared {
        ruleset: &ruleset,
        table: &table,
        game: Some(Game::Hoi4),
        type_index: Some(&index),
        modifier_keys: None,
        loc_index: None,
        extra_loc_keys: None,
        inline_scripts: None,
        registry: registry.as_ref(),
        scope_checks: true,
        var_checks: false,
    };

    // Warm lazily initialized validation state outside the measurement.
    let warm_errors = validate_prepared(&parsed, "game/common/foo/test.txt", &prepared);
    assert!(
        warm_errors.is_empty(),
        "warm validation errors: {warm_errors:?}"
    );
    SCOPE_ID_SIZED_ALLOCS.store(0, Ordering::Relaxed);
    TRACKING.store(true, Ordering::SeqCst);
    let errors = validate_prepared(&parsed, "game/common/foo/test.txt", &prepared);
    TRACKING.store(false, Ordering::SeqCst);
    assert!(errors.is_empty(), "got errors: {errors:?}");
    SCOPE_ID_SIZED_ALLOCS.load(Ordering::Relaxed)
}

#[test]
fn scope_probe_allocations_do_not_grow_with_target_fields() {
    let one_target = count_scope_sized_allocations(1);
    let many_targets = count_scope_sized_allocations(64);
    assert_eq!(
        many_targets, one_target,
        "scope target probes allocated one-entry ScopeId vectors per field"
    );
}
