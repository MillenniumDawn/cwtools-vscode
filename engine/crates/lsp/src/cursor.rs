use std::sync::Arc;

use cwtools_info::{PositionElement, ReferenceHint};
use cwtools_rules::rules_types::{NewField, RuleSet, RuleType, TypeType, ValueType};

pub(crate) struct CursorResolution {
    pub(crate) rctx: cwtools_validation::position::RuleContext,
    pub(crate) ruleset: Arc<RuleSet>,
}

pub(crate) struct RuleCursorInfo {
    pub(crate) element: PositionElement,
    pub(crate) hint: ReferenceHint,
    pub(crate) category: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) required_scopes: Vec<String>,
    pub(crate) current_scope: Option<String>,
    pub(crate) root_scope: Option<String>,
    pub(crate) prev_scope: Option<String>,
    pub(crate) from_scopes: Vec<String>,
    /// setting is on and it differs from the current scope. (#37)
    pub(crate) resolved_scope: Option<String>,
}

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

pub(crate) fn hint_from_rule_left(rule_type: &RuleType, key: &str) -> ReferenceHint {
    let left = match rule_type {
        RuleType::LeafRule { left, .. } => left,
        RuleType::NodeRule { left, .. } => left,
        _ => return ReferenceHint::Unknown,
    };
    match left {
        NewField::TypeField(_) | NewField::ValueField(ValueType::Enum(_)) => {
            field_to_hint_simple(left, key)
        }
        _ => ReferenceHint::Unknown,
    }
}

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

pub(crate) fn scope_link_key_type(
    ruleset: &RuleSet,
    type_index: &cwtools_info::TypeIndex,
    key: &str,
) -> Option<String> {
    for li in &ruleset.link_inputs {
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
