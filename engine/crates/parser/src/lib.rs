pub mod ast;
pub mod fix;
pub mod format;
pub mod parser;

/// Strip one layer of surrounding double quotes, if present.
#[inline]
pub fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::unquote;

    #[test]
    fn unquote_strips_paired_quotes_and_leaves_lone_quotes() {
        for (input, expected) in [
            ("", ""),
            ("value", "value"),
            ("\"value\"", "value"),
            ("\"\"", ""),
            ("\"", "\""),
            ("\"value", "\"value"),
            ("value\"", "value\""),
        ] {
            assert_eq!(unquote(input), expected, "unquote({input:?})");
        }
    }
}
