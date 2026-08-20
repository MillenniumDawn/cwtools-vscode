use std::sync::Arc;

use cwtools_info::{PositionElement, ReferenceHint};
use cwtools_rules::rules_types::{NewField, RuleSet, RuleType, TypeType, ValueType};

/// The resolved rule context at the cursor plus the ruleset snapshot it was
/// resolved against, returned by [`crate::Backend::resolve_at_cursor`]. The Arcs
/// keep the ruleset/registry alive for callers that inspect the context after
/// the guards are dropped.
pub(crate) struct CursorResolution {
    pub(crate) rctx: cwtools_validation::position::RuleContext,
    pub(crate) ruleset: Arc<RuleSet>,
}

/// What `rule_info_at_cursor` resolves for the leaf under the cursor.
pub(crate) struct RuleCursorInfo {
    pub(crate) element: PositionElement,
    pub(crate) hint: ReferenceHint,
    /// Alias category the key resolves through (`trigger`, `effect`, …), for
    /// the hover header.
    pub(crate) category: Option<String>,
    /// The matched rule's `###` description.
    pub(crate) description: Option<String>,
    pub(crate) required_scopes: Vec<String>,
    /// The scope context at the cursor (the scope the block evaluates in), for
    /// the hover. `None` when no registry or the scope is the `any` wildcard.
    pub(crate) current_scope: Option<String>,
    /// Related scopes at the cursor, for the hover scope table. ROOT is the
    /// outermost block's scope; PREV is the enclosing scope (one level out).
    /// Each is `None` when absent or a suppressed placeholder.
    pub(crate) root_scope: Option<String>,
    pub(crate) prev_scope: Option<String>,
    /// The FROM chain: `[0]` = FROM, `[1]` = FROM.FROM, … (placeholders dropped).
    pub(crate) from_scopes: Vec<String>,
    /// The scope the hovered key resolves to (run through `change_scope`). Shown
    /// as a `Resolves to` line only when the `hover.scopeDisplay = "resolved"`
    /// setting is on and it differs from the current scope. (#37)
    pub(crate) resolved_scope: Option<String>,
}

/// Map a matched leaf rule's right-hand field to a [`ReferenceHint`] for the
/// leaf's value (the same classification `info_at_position` used to do at
/// depth 0-1, now fed by the full position resolver).
pub(crate) fn hint_from_rule_right(
    rule_type: &RuleType,
    value: &str,
    ruleset: &RuleSet,
) -> ReferenceHint {
    let right = match rule_type {
        RuleType::LeafRule { right, .. } => right,
        RuleType::LeafValueRule { right } => right,
        _ => return ReferenceHint::Unknown,
    };
    field_to_hint(right, value, ruleset)
}

/// Map a matched rule's LEFT field to a [`ReferenceHint`] for the key — for
/// references that sit on the key, like a `<character>` used as a scoped-trigger
/// block key or a `type[…]` entity-definition key.
pub(crate) fn hint_from_rule_left(rule_type: &RuleType, key: &str) -> ReferenceHint {
    let left = match rule_type {
        RuleType::LeafRule { left, .. } => left,
        RuleType::NodeRule { left, .. } => left,
        _ => return ReferenceHint::Unknown,
    };
    match left {
        NewField::TypeField(_) | NewField::ValueField(ValueType::Enum(_)) => {
            // No ruleset needed for the type/enum cases; the scope-link upgrade
            // only applies to right-hand values, so pass an empty ruleset.
            field_to_hint_simple(left, key)
        }
        _ => ReferenceHint::Unknown,
    }
}

/// Shared field → hint mapping for the type/enum cases that don't need the
/// ruleset (used by the key-side classifier).
fn field_to_hint_simple(field: &NewField, value: &str) -> ReferenceHint {
    match field {
        NewField::TypeField(TypeType::Simple(t)) => ReferenceHint::TypeRef {
            type_name: t.clone(),
            value: value.to_string(),
        },
        NewField::TypeField(TypeType::Complex {
            prefix,
            name,
            suffix,
        }) => {
            let inner = value
                .strip_prefix(prefix.as_str())
                .unwrap_or(value)
                .strip_suffix(suffix.as_str())
                .unwrap_or(value);
            ReferenceHint::TypeRef {
                type_name: name.clone(),
                value: inner.to_string(),
            }
        }
        NewField::ValueField(ValueType::Enum(e)) => ReferenceHint::EnumRef {
            enum_name: e.clone(),
            value: value.to_string(),
        },
        _ => ReferenceHint::Unknown,
    }
}

/// Full field → hint mapping for a right-hand value. Resolves a prefixed scope
/// reference (e.g. `sp:sp_nuclear_reactor`) to a `TypeRef` via the matching
/// link's `data_source` `<type>`, so goto/hover treat the value as that instance.
fn field_to_hint(field: &NewField, value: &str, ruleset: &RuleSet) -> ReferenceHint {
    match field {
        NewField::LocalisationField { .. } => ReferenceHint::LocRef {
            key: value.to_string(),
        },
        NewField::FilepathField { .. } => ReferenceHint::FileRef {
            path: value.to_string(),
        },
        NewField::VariableGetField(ns) => ReferenceHint::Variable {
            name: value.to_string(),
            namespace: ns.clone(),
        },
        NewField::ScopeField(_) => {
            scope_prefixed_type_ref(value, ruleset).unwrap_or_else(|| ReferenceHint::ScopeName {
                name: value.to_string(),
            })
        }
        other => field_to_hint_simple(other, value),
    }
}

/// A prefixed scope reference like `sp:sp_nuclear_reactor` resolves through the
/// link whose `prefix` matches (`sp` → `prefix = sp:`, `data_source =
/// <special_project>`). Strip the prefix and point at the data-source type. The
/// scope-field's scope NAME (`special_project`) is a scope type, not the link
/// name, so matching must be by value prefix.
fn scope_prefixed_type_ref(value: &str, ruleset: &RuleSet) -> Option<ReferenceHint> {
    for li in &ruleset.link_inputs {
        let prefix = li.prefix.as_deref()?;
        if prefix.is_empty() {
            continue;
        }
        let Some(rest) = value.strip_prefix(prefix) else {
            continue;
        };
        for ds in &li.data_source {
            if let Some(t) = ds.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
                return Some(ReferenceHint::TypeRef {
                    type_name: t.to_string(),
                    value: rest.to_string(),
                });
            }
        }
    }
    None
}

/// A bare key that is a known instance of a type used as a prefix-less link
/// `data_source` (e.g. a character name, where the `character` link's
/// `data_source` is `<character>`). Returns the type name so the key resolves to
/// its definition. Used for keys that scope into an entity without a rule match.
pub(crate) fn scope_link_key_type(
    ruleset: &RuleSet,
    type_index: &cwtools_info::TypeIndex,
    key: &str,
) -> Option<String> {
    for li in &ruleset.link_inputs {
        // A bare key carries no prefix, so only prefix-less links apply.
        if li.prefix.is_some() {
            continue;
        }
        for ds in &li.data_source {
            if let Some(t) = ds.strip_prefix('<').and_then(|s| s.strip_suffix('>'))
                && type_index
                    .instances(t)
                    .iter()
                    .any(|(_, inst)| inst.name == key)
            {
                return Some(t.to_string());
            }
        }
    }
    None
}
