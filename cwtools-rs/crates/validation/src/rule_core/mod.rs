//! The rule-vs-AST core: matching children against rules, cardinality,
//! alias-usage resolution, and per-field value checks.

mod alias;
mod children;
mod leaf;
mod matching;
mod subtype_merge;
mod suggest;

pub(crate) use alias::{alias_overloads, alias_overloads_with_confidence};
pub(crate) use children::{math_clause_rules, rule_right_is_math_expr};
pub(crate) use leaf::{field_matches_value, is_builtin_variable};
pub(crate) use matching::{matching_candidates, rule_matches_leaf_key};
pub(crate) use subtype_merge::{
    flatten_nested_subtype_rules, merged_rules_for_type, validate_with_type,
};

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, Once};

    use tracing::span::{Attributes, Id, Record};
    use tracing::subscriber::Interest;
    use tracing::{Event, Level, Metadata, Subscriber};

    use crate::{Prepared, validate_prepared};
    use cwtools_parser::parser::parse_string;
    use cwtools_rules::rules_converter::ast_to_ruleset;
    use cwtools_string_table::string_table::StringTable;

    fn span_interest(metadata: &Metadata<'_>) -> Interest {
        if metadata.is_span() {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    /// Interest is computed from the *global* dispatcher. A thread-local
    /// collector never turns TRACE callsites on after another test registered
    /// them as disabled, so this no-op subscriber has to sit on the process.
    struct EnableSpans;

    impl Subscriber for EnableSpans {
        fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
            span_interest(metadata)
        }
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.is_span()
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    fn enable_span_interest() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = tracing::subscriber::set_global_default(EnableSpans);
            tracing::callsite::rebuild_interest_cache();
        });
    }

    struct SpanLog {
        next: AtomicU64,
        spans: Arc<Mutex<Vec<(&'static str, Level)>>>,
    }

    impl Subscriber for SpanLog {
        fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
            span_interest(metadata)
        }

        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.is_span()
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            self.spans
                .lock()
                .expect("span log")
                .push((span.metadata().name(), *span.metadata().level()));
            Id::from_u64(self.next.fetch_add(1, Ordering::Relaxed))
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    fn level_of(spans: &[(&str, Level)], name: &str) -> Option<Level> {
        spans
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, level)| *level)
    }

    #[test]
    fn inner_loop_spans_are_trace() {
        enable_span_interest();
        let table = StringTable::new();
        let ruleset = ast_to_ruleset(
            &parse_string(
                r#"
types = { type[foo] = { path = "game/common/foo" } }
foo = {
    cost = int
    alias_name[effect] = alias_match_left[effect]
}
alias[effect:add_pp] = int
"#,
                &table,
            ),
            &table,
        );
        let parsed = parse_string("foo = { cost = 1 add_pp = 10 }", &table);
        let prepared = Prepared {
            ruleset: &ruleset,
            table: &table,
            game: None,
            type_index: None,
            modifier_keys: None,
            loc_index: None,
            extra_loc_keys: None,
            inline_scripts: None,
            registry: None,
            scope_checks: false,
            var_checks: false,
        };

        let spans = Arc::new(Mutex::new(Vec::new()));
        let log = SpanLog {
            next: AtomicU64::new(1),
            spans: Arc::clone(&spans),
        };
        tracing::subscriber::with_default(log, || {
            tracing::callsite::rebuild_interest_cache();
            let _ = validate_prepared(&parsed, "game/common/foo/test.txt", &prepared);
        });
        let spans = spans.lock().expect("span log").clone();

        assert_eq!(level_of(&spans, "validate_prepared"), Some(Level::INFO));
        assert_eq!(
            level_of(&spans, "count_and_validate_children"),
            Some(Level::TRACE)
        );
        assert_eq!(level_of(&spans, "validate_leaf"), Some(Level::TRACE));
        assert_eq!(level_of(&spans, "validate_alias_usage"), Some(Level::TRACE));
    }
}
