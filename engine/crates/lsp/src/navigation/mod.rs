//! Navigation: goto, references, rename, symbols, folding/selection.
mod goto;
mod helpers;
mod references;
mod rename;
mod structure;
mod symbols;
mod use_sites;

pub(crate) use helpers::{
    TopSymbols, at_var_at_cursor, brace_folding_ranges, brace_pairs, build_doc_symbols,
    code_token_cols_in_line, comment_and_region_folds, cwt_ref_at, dedup_locations, highlight_kind,
    locations_at, locations_at_with_texts, make_symbol, member_pos_in_block, prepare_rename_range,
    rename_refused, resolve_file_ref, selection_spans, source_range_in_text,
    source_range_without_text, symbol_rank, unquote, value_col_in_line, value_start_after_eq,
    word_at_position,
};
pub(crate) use use_sites::scan_use_sites;
