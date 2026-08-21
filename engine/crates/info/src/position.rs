use cwtools_parser::ast::{Arena, Child, ParsedFile, Value};
use cwtools_string_table::string_table::StringTable;

use crate::leaf_value_string;

/// A hint about what kind of reference a leaf's value or key represents.
/// Used by the LSP for hover text and goto-definition. Populated from the
/// matched rule's right-hand side (the LSP's `hint_from_rule_right`, fed by
/// `cwtools_validation::position::rules_at_pos`).
#[derive(Debug, Clone)]
pub enum ReferenceHint {
    /// The value is a reference to an instance of `type_name`.
    TypeRef { type_name: String, value: String },
    /// The value is a localisation key.
    LocRef { key: String },
    /// The value is a member of enum `enum_name`.
    EnumRef { enum_name: String, value: String },
    /// The key/value is a file path.
    FileRef { path: String },
    /// The value is a scope name.
    ScopeName { name: String },
    /// The value is a read of a defined script variable (`value[variable]`).
    Variable { name: String, namespace: String },
    /// Classification was not possible with current rule depth.
    Unknown,
}

/// Which kind of AST element is at the cursor.
#[derive(Debug, Clone)]
pub enum PositionElement {
    /// A `key = value` leaf (a `key = { … }` clause reports an empty value).
    Leaf { key: String, value: String },
    /// A bare value inside a clause (no key).
    LeafValue { value: String },
}

/// Find the AST element at `(line, col)` without rule classification.
/// Use this when no ruleset is available or only the key/value is needed.
pub fn element_at_position(
    file: &ParsedFile,
    line: u32,
    col: u16,
    table: &StringTable,
) -> Option<PositionElement> {
    let target = cwtools_parser::ast::SourcePos { line, col };
    find_element_in_children(&file.root_children, &file.arena, &target, table)
}

fn find_element_in_children(
    children: &[Child],
    arena: &Arena,
    target: &cwtools_parser::ast::SourcePos,
    table: &StringTable,
) -> Option<PositionElement> {
    for child in children {
        match child {
            Child::Leaf(idx) => {
                let leaf = &arena.leaves[*idx as usize];
                if pos_in_range(target, &leaf.pos) {
                    let key = table.get_string(leaf.key.normal).unwrap_or_default();
                    let value = leaf_value_string(&leaf.value, table);
                    if let Value::Clause(ch) = &leaf.value
                        && let Some(inner) = find_element_in_children(ch, arena, target, table)
                    {
                        return Some(inner);
                    }
                    return Some(PositionElement::Leaf { key, value });
                }
            }
            Child::LeafValue(idx) => {
                let lv = &arena.leaf_values[*idx as usize];
                if pos_in_range(target, &lv.pos) {
                    let value = leaf_value_string(&lv.value, table);
                    return Some(PositionElement::LeafValue { value });
                }
            }
            _ => {}
        }
    }
    None
}

fn pos_in_range(
    pos: &cwtools_parser::ast::SourcePos,
    range: &cwtools_parser::ast::SourceRange,
) -> bool {
    let start = &range.start;
    let end = &range.end;
    if pos.line < start.line || pos.line > end.line {
        return false;
    }
    if pos.line == start.line && pos.col < start.col {
        return false;
    }
    if pos.line == end.line && pos.col > end.col {
        return false;
    }
    true
}
