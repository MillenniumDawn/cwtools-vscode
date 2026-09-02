use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use cwtools_parser::ast::{Arena, Child, ParsedFile, SourceRange, Value};
use cwtools_rules::rules_types::{Options, RuleType, ValueType};
use cwtools_string_table::string_table::StringTable;
use cwtools_validation::Prepared;
use cwtools_validation::position::value_rules_for_key;

use crate::Backend;
use crate::paths::{position_byte_index, source_column_to_lsp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Space {
    RgbFloat,
    RgbInt,
    HsvFloat,
    Hsv360,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Convention {
    pub(crate) space: Space,
    rgb_prefix: bool,
}

impl Convention {
    const fn new(space: Space) -> Self {
        Convention {
            space,
            rgb_prefix: false,
        }
    }

    fn prefix(self) -> &'static str {
        match self.space {
            Space::RgbFloat | Space::RgbInt if self.rgb_prefix => "rgb ",
            Space::RgbFloat | Space::RgbInt => "",
            Space::HsvFloat => "hsv ",
            Space::Hsv360 => "hsv360 ",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColourLiteral {
    pub(crate) convention: Convention,
    pub(crate) channels: [f32; 3],
}

pub(crate) fn parse_literal(text: &str) -> Option<ColourLiteral> {
    let trimmed = text.trim();
    let brace = trimmed.find('{')?;
    let (prefix, rest) = (
        trimmed[..brace].trim().to_ascii_lowercase(),
        &trimmed[brace..],
    );
    let body = rest.strip_prefix('{')?.strip_suffix('}')?;
    let body: String = body
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    let parts: Vec<&str> = body.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let mut channels = [0f32; 3];
    for (slot, part) in channels.iter_mut().zip(&parts) {
        *slot = part.parse::<f32>().ok()?;
    }
    let has_decimal = parts.iter().any(|p| p.contains('.'));
    let looks_float = has_decimal || channels.iter().all(|c| *c <= 1.0);
    let convention = match prefix.as_str() {
        "hsv360" => Convention::new(Space::Hsv360),
        "hsv" => Convention::new(Space::HsvFloat),
        "rgb" => Convention {
            space: if has_decimal {
                Space::RgbFloat
            } else {
                Space::RgbInt
            },
            rgb_prefix: true,
        },
        "" if looks_float => Convention::new(Space::RgbFloat),
        "" => Convention::new(Space::RgbInt),
        _ => return None,
    };
    Some(ColourLiteral {
        convention,
        channels,
    })
}

pub(crate) fn to_color(lit: &ColourLiteral) -> Color {
    let [a, b, c] = lit.channels;
    let (red, green, blue) = match lit.convention.space {
        Space::RgbFloat => (a, b, c),
        Space::RgbInt => (a / 255.0, b / 255.0, c / 255.0),
        Space::HsvFloat => hsv_to_rgb(a, b, c),
        Space::Hsv360 => hsv_to_rgb(a / 360.0, b / 100.0, c / 100.0),
    };
    Color {
        red: red.clamp(0.0, 1.0),
        green: green.clamp(0.0, 1.0),
        blue: blue.clamp(0.0, 1.0),
        alpha: 1.0,
    }
}

pub(crate) fn format_literal(color: &Color, convention: Convention) -> String {
    let (r, g, b) = (
        color.red.clamp(0.0, 1.0),
        color.green.clamp(0.0, 1.0),
        color.blue.clamp(0.0, 1.0),
    );
    let body = match convention.space {
        Space::RgbFloat => format!("{:.3} {:.3} {:.3}", r, g, b),
        Space::RgbInt => format!(
            "{} {} {}",
            (r * 255.0).round() as u32,
            (g * 255.0).round() as u32,
            (b * 255.0).round() as u32
        ),
        Space::HsvFloat => {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            format!("{:.3} {:.3} {:.3}", h, s, v)
        }
        Space::Hsv360 => {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            format!(
                "{} {} {}",
                (h * 360.0).round() as u32,
                (s * 100.0).round() as u32,
                (v * 100.0).round() as u32
            )
        }
    };
    format!("{}{{ {} }}", convention.prefix(), body)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(1.0) * 6.0;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let sector = h.floor();
    let f = h - sector;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match sector as u32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    let s = if max == 0.0 { 0.0 } else { delta / max };
    (h.rem_euclid(1.0), s, max)
}

fn is_colour_rule(rule: &RuleType) -> bool {
    let RuleType::NodeRule { rules, .. } = rule else {
        return false;
    };
    !rules.is_empty()
        && rules.iter().all(|(rt, opts)| {
            opts.min == 3
                && matches!(
                    rt,
                    RuleType::LeafValueRule {
                        right: cwtools_rules::rules_types::NewField::ValueField(
                            ValueType::Int { .. } | ValueType::Float { .. }
                        )
                    }
                )
        })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FoundColour {
    pub(crate) range: SourceRange,
    pub(crate) literal: ColourLiteral,
}

pub(crate) fn document_colours(
    file: &ParsedFile,
    table: &StringTable,
    text: &str,
    rules: Option<(&Prepared<'_>, &str)>,
) -> Vec<FoundColour> {
    let mut out = Vec::new();
    let Some((prepared, logical_path)) = rules else {
        return out;
    };
    let cx = Cx {
        ast: file,
        arena: &file.arena,
        table,
        lines: text.lines().collect(),
        prepared,
        logical_path,
    };
    let root_rules =
        crate::semantic::block_rules_for(file, prepared, logical_path, &file.root_children);
    collect(&file.root_children, &cx, root_rules.as_deref(), &mut out);
    out
}

struct Cx<'a> {
    ast: &'a ParsedFile,
    arena: &'a Arena,
    table: &'a StringTable,
    lines: Vec<&'a str>,
    prepared: &'a Prepared<'a>,
    logical_path: &'a str,
}

fn collect(
    children: &[Child],
    cx: &Cx<'_>,
    block_rules: Option<&[(RuleType, Options)]>,
    out: &mut Vec<FoundColour>,
) {
    for child in children {
        let Child::Leaf(idx) = child else { continue };
        let leaf = &cx.arena.leaves[*idx as usize];
        let Value::Clause(inner) = &leaf.value else {
            continue;
        };
        let raw_key = cx.table.get_string(leaf.key.normal).unwrap_or_default();
        let matched = block_rules.map_or_else(Vec::new, |rules| {
            value_rules_for_key(
                cx.prepared.ruleset,
                cx.prepared.type_index,
                rules,
                raw_key.trim_matches('"'),
            )
        });
        if matched.iter().any(|(rt, _)| is_colour_rule(rt))
            && let Some(found) = read_colour(leaf.pos, &raw_key, &cx.lines)
        {
            out.push(found);
            continue;
        }
        let inner_rules: Vec<(RuleType, Options)> = match block_rules {
            None => crate::semantic::block_rules_for(cx.ast, cx.prepared, cx.logical_path, inner)
                .unwrap_or_default(),
            Some(_) => matched
                .iter()
                .filter_map(|(rt, _)| match rt {
                    RuleType::NodeRule { rules, .. } => Some(rules.iter().cloned()),
                    _ => None,
                })
                .flatten()
                .collect(),
        };
        if !inner_rules.is_empty() {
            collect(inner, cx, Some(&inner_rules), out);
        }
    }
}

fn read_colour(pos: SourceRange, raw_key: &str, lines: &[&str]) -> Option<FoundColour> {
    let start_line = pos.start.line.saturating_sub(1) as usize;
    let key_end = pos.start.col as usize + raw_key.chars().count();
    let eq = lines
        .get(start_line)?
        .chars()
        .enumerate()
        .skip(key_end)
        .find(|(_, c)| *c == '=')
        .map(|(i, _)| i + 1)?;

    let mut text = String::new();
    let mut value_start: Option<(usize, usize)> = None;
    let mut depth = 0usize;
    for (line_no, line) in lines.iter().enumerate().skip(start_line) {
        let from = if line_no == start_line { eq } else { 0 };
        for (col, ch) in line.chars().enumerate().skip(from) {
            if value_start.is_none() {
                if ch.is_whitespace() {
                    continue;
                }
                if ch == '#' {
                    break;
                }
                value_start = Some((line_no, col));
            }
            text.push(ch);
            match ch {
                '{' => depth += 1,
                '}' if depth == 0 => return None,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let (sl, sc) = value_start?;
                        return parse_literal(&text).map(|literal| FoundColour {
                            range: SourceRange {
                                start: cwtools_parser::ast::SourcePos {
                                    line: sl as u32 + 1,
                                    col: sc as u16,
                                },
                                end: cwtools_parser::ast::SourcePos {
                                    line: line_no as u32 + 1,
                                    col: col as u16 + 1,
                                },
                            },
                            literal,
                        });
                    }
                }
                _ => {}
            }
        }
        if value_start.is_some() {
            text.push(' ');
        }
    }
    None
}

/// negotiated encoding, against the already-split lines.
fn to_lsp_range(range: SourceRange, lines: &[&str], encoding: &PositionEncodingKind) -> Range {
    let conv = |line: u32, col: u16| {
        let line0 = line.saturating_sub(1);
        let text = lines.get(line0 as usize).copied().unwrap_or("");
        Position::new(line0, source_column_to_lsp(text, col as u32, encoding))
    };
    Range {
        start: conv(range.start.line, range.start.col),
        end: conv(range.end.line, range.end.col),
    }
}

pub(crate) fn text_in_range(text: &str, range: Range, encoding: &PositionEncodingKind) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    for line_no in range.start.line..=range.end.line {
        let Some(line) = lines.get(line_no as usize) else {
            break;
        };
        let from = if line_no == range.start.line {
            position_byte_index(line, range.start.character, encoding)
        } else {
            0
        };
        let to = if line_no == range.end.line {
            position_byte_index(line, range.end.character, encoding)
        } else {
            line.len()
        };
        if from <= to && to <= line.len() {
            out.push_str(&line[from..to]);
        }
        if line_no != range.end.line {
            out.push(' ');
        }
    }
    out
}

impl Backend {
    pub(crate) async fn document_color_impl(
        &self,
        params: DocumentColorParams,
    ) -> Result<Vec<ColorInformation>> {
        let uri = params.text_document.uri.to_string();
        if crate::paths::has_loc_ext(&uri) || crate::paths::is_cwt_file(&uri) {
            return Ok(Vec::new());
        }
        let Some(ast) = self.ast_for(&uri) else {
            return Ok(Vec::new());
        };
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(Vec::new());
        };
        let (game, scope_checks, var_checks, encoding, ws_prefix) = {
            let cfg = self.state.config.read();
            (
                cfg.game(),
                cfg.scope_checks,
                cfg.var_checks,
                cfg.position_encoding.clone(),
                cfg.workspace_prefix.clone(),
            )
        };
        let logical_path = crate::paths::logical_path_from_uri(&uri, &ws_prefix);
        let (ruleset, modifier_keys, scope_registry) = {
            let rules = self.state.rules.read();
            (
                rules.ruleset.clone(),
                rules.modifier_keys.clone(),
                rules.scope_registry.clone(),
            )
        };
        let Some(ruleset) = ruleset else {
            return Ok(Vec::new());
        };

        let found = {
            let info = self.state.info_service.read();
            let inline_guard = self.state.inline_scripts.read();
            let prepared = crate::validate::make_prepared(
                &ruleset,
                &self.state.string_table,
                game,
                &info.type_index,
                &modifier_keys,
                None,
                None,
                Some(&inline_guard),
                scope_registry.as_ref(),
                scope_checks,
                var_checks,
            );
            document_colours(
                &ast,
                &self.state.string_table,
                &text,
                Some((&prepared, logical_path.as_str())),
            )
        };

        let lines: Vec<&str> = text.lines().collect();
        Ok(found
            .iter()
            .map(|f| ColorInformation {
                range: to_lsp_range(f.range, &lines, &encoding),
                color: to_color(&f.literal),
            })
            .collect())
    }

    pub(crate) async fn color_presentation_impl(
        &self,
        params: ColorPresentationParams,
    ) -> Result<Vec<ColorPresentation>> {
        let uri = params.text_document.uri.to_string();
        let Some(text) = self.file_text_for(&uri).await else {
            return Ok(Vec::new());
        };
        let encoding = self.state.config.read().position_encoding.clone();
        let source = text_in_range(&text, params.range, &encoding);
        let Some(literal) = parse_literal(&source) else {
            return Ok(Vec::new());
        };
        let new_text = format_literal(&params.color, literal.convention);
        Ok(vec![ColorPresentation {
            label: new_text.clone(),
            text_edit: Some(TextEdit {
                range: params.range,
                new_text,
            }),
            additional_text_edits: None,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> ColourLiteral {
        parse_literal(text).unwrap_or_else(|| panic!("{text:?} should parse as a colour"))
    }

    #[test]
    fn floats_with_a_decimal_point_are_rgb_floats() {
        let l = lit("{ 0.2 0.4 0.6 }");
        assert_eq!(l.convention, Convention::new(Space::RgbFloat));
        assert_eq!(l.channels, [0.2, 0.4, 0.6]);
    }

    #[test]
    fn integers_above_one_are_rgb_bytes() {
        let l = lit("{ 51 102 153 }");
        assert_eq!(l.convention, Convention::new(Space::RgbInt));
        assert_eq!(l.channels, [51.0, 102.0, 153.0]);
    }

    #[test]
    fn bare_integers_within_zero_to_one_read_as_floats() {
        assert_eq!(lit("{ 1 0 0 }").convention.space, Space::RgbFloat);
        assert_eq!(lit("{ 1 1 1 }").convention.space, Space::RgbFloat);
    }

    #[test]
    fn an_explicit_prefix_wins_over_the_number_shape() {
        assert_eq!(lit("rgb { 51 102 153 }").convention.space, Space::RgbInt);
        assert_eq!(lit("rgb { 0.2 0.4 0.6 }").convention.space, Space::RgbFloat);
        assert_eq!(lit("hsv { 0.5 1.0 1.0 }").convention.space, Space::HsvFloat);
        assert_eq!(lit("hsv360 { 340 60 55 }").convention.space, Space::Hsv360);
        assert_eq!(lit("HSV { 0.5 1.0 1.0 }").convention.space, Space::HsvFloat);
    }

    #[test]
    fn the_rgb_keyword_is_remembered_separately_from_the_space() {
        assert_ne!(
            lit("rgb { 51 102 153 }").convention,
            lit("{ 51 102 153 }").convention
        );
        assert_eq!(
            lit("rgb { 51 102 153 }").convention.space,
            lit("{ 51 102 153 }").convention.space
        );
    }

    #[test]
    fn non_colour_shapes_do_not_parse() {
        assert!(parse_literal("{ 1 2 }").is_none(), "two channels");
        assert!(parse_literal("{ 1 2 3 4 }").is_none(), "four channels");
        assert!(parse_literal("{ a b c }").is_none(), "not numbers");
        assert!(parse_literal("0.2 0.4 0.6").is_none(), "no braces");
        assert!(parse_literal("").is_none());
        assert!(
            parse_literal("cmyk { 1 2 3 }").is_none(),
            "an unknown keyword must not be silently dropped"
        );
    }

    #[test]
    fn a_comment_inside_the_block_is_ignored() {
        let l = lit("{ 51 102 153 # dark blue\n}");
        assert_eq!(l.channels, [51.0, 102.0, 153.0]);
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.005
    }

    #[test]
    fn byte_and_float_spellings_of_one_colour_give_the_same_swatch() {
        let bytes = to_color(&lit("{ 51 102 153 }"));
        let floats = to_color(&lit("{ 0.2 0.4 0.6 }"));
        assert!(approx(bytes.red, floats.red), "{bytes:?} vs {floats:?}");
        assert!(approx(bytes.green, floats.green));
        assert!(approx(bytes.blue, floats.blue));
        assert_eq!(bytes.alpha, 1.0);
    }

    #[test]
    fn hsv_converts_to_the_expected_rgb() {
        let red = to_color(&lit("hsv { 0.0 1.0 1.0 }"));
        assert_eq!((red.red, red.green, red.blue), (1.0, 0.0, 0.0));
        let green = to_color(&lit("hsv360 { 120 100 100 }"));
        assert!(approx(green.red, 0.0) && approx(green.green, 1.0) && approx(green.blue, 0.0));
        let grey = to_color(&lit("hsv { 0.3 0.0 0.5 }"));
        assert!(approx(grey.red, 0.5) && approx(grey.green, 0.5) && approx(grey.blue, 0.5));
    }

    #[test]
    fn out_of_range_channels_are_clamped_into_the_swatch() {
        let over = to_color(&lit("{ 300 -20 128 }"));
        assert_eq!(over.red, 1.0);
        assert_eq!(over.green, 0.0);
        assert!(approx(over.blue, 128.0 / 255.0));
    }

    fn round_trip(source: &str) -> (Convention, String, ColourLiteral) {
        let original = lit(source);
        let color = to_color(&original);
        let written = format_literal(&color, original.convention);
        let reread = lit(&written);
        (original.convention, written, reread)
    }

    #[test]
    fn float_rgb_round_trips_as_float_rgb() {
        let (conv, written, reread) = round_trip("{ 0.2 0.4 0.6 }");
        assert_eq!(conv, Convention::new(Space::RgbFloat));
        assert_eq!(written, "{ 0.200 0.400 0.600 }");
        assert_eq!(reread.convention, Convention::new(Space::RgbFloat));
        assert_eq!(reread.channels, [0.2, 0.4, 0.6]);
    }

    #[test]
    fn int_rgb_round_trips_as_int_rgb() {
        let (conv, written, reread) = round_trip("{ 51 102 153 }");
        assert_eq!(conv, Convention::new(Space::RgbInt));
        assert_eq!(written, "{ 51 102 153 }");
        assert_eq!(reread.convention, Convention::new(Space::RgbInt));
        assert_eq!(reread.channels, [51.0, 102.0, 153.0]);
    }

    #[test]
    fn prefixed_conventions_keep_their_prefix() {
        assert_eq!(round_trip("rgb { 51 102 153 }").1, "rgb { 51 102 153 }");
        assert_eq!(
            round_trip("rgb { 0.2 0.4 0.6 }").1,
            "rgb { 0.200 0.400 0.600 }"
        );
        assert_eq!(
            round_trip("hsv { 0.5 1.0 1.0 }").1,
            "hsv { 0.500 1.000 1.000 }"
        );
        assert_eq!(round_trip("hsv360 { 340 60 55 }").1, "hsv360 { 340 60 55 }");
    }

    #[test]
    fn a_literal_that_writes_no_prefix_does_not_gain_one() {
        assert_eq!(round_trip("{ 51 102 153 }").1, "{ 51 102 153 }");
        assert_eq!(round_trip("{ 0.2 0.4 0.6 }").1, "{ 0.200 0.400 0.600 }");
    }

    #[test]
    fn the_swatch_survives_a_round_trip_in_every_convention() {
        for source in [
            "{ 0.2 0.4 0.6 }",
            "{ 51 102 153 }",
            "rgb { 200 40 90 }",
            "hsv { 0.75 0.6 0.9 }",
            "hsv360 { 340 60 55 }",
        ] {
            let before = to_color(&lit(source));
            let (_, written, reread) = round_trip(source);
            let after = to_color(&reread);
            assert!(
                approx(before.red, after.red)
                    && approx(before.green, after.green)
                    && approx(before.blue, after.blue),
                "{source} -> {written}: {before:?} became {after:?}"
            );
        }
    }

    #[test]
    fn picking_a_new_colour_writes_the_conventions_own_spelling() {
        let pick = Color {
            red: 1.0,
            green: 0.5,
            blue: 0.0,
            alpha: 1.0,
        };
        assert_eq!(
            format_literal(&pick, Convention::new(Space::RgbFloat)),
            "{ 1.000 0.500 0.000 }"
        );
        assert_eq!(
            format_literal(&pick, Convention::new(Space::RgbInt)),
            "{ 255 128 0 }"
        );
        assert_eq!(
            format_literal(&pick, Convention::new(Space::HsvFloat)),
            "hsv { 0.083 1.000 1.000 }"
        );
        assert_eq!(
            format_literal(&pick, Convention::new(Space::Hsv360)),
            "hsv360 { 30 100 100 }"
        );
    }

    fn find(text: &str, key: &str) -> FoundColour {
        let table = StringTable::new();
        let ast = cwtools_parser::parser::parse_string(text, &table);
        let lines: Vec<&str> = text.lines().collect();
        let Child::Leaf(root) = ast.root_children[0] else {
            panic!("expected a root clause")
        };
        let Value::Clause(inner) = &ast.arena.leaves[root as usize].value else {
            panic!("expected a clause")
        };
        for c in inner {
            if let Child::Leaf(i) = c {
                let leaf = &ast.arena.leaves[*i as usize];
                if table.get_string(leaf.key.normal).as_deref() == Some(key) {
                    return read_colour(leaf.pos, key, &lines).expect("colour span");
                }
            }
        }
        panic!("no {key} leaf");
    }

    #[test]
    fn the_span_covers_the_value_only_not_the_key() {
        let text = "c = {\n    color = { 0.2 0.4 0.6 }\n}\n";
        let found = find(text, "color");
        assert_eq!(found.range.start.line, 2);
        assert_eq!(found.range.start.col, 12, "just past `color = `");
        assert_eq!(found.range.end.col, 27, "one past the closing brace");
        assert_eq!(found.literal.convention.space, Space::RgbFloat);
        let line: Vec<char> = text.lines().nth(1).unwrap().chars().collect();
        let slice: String = line[12..27].iter().collect();
        assert_eq!(slice, "{ 0.2 0.4 0.6 }");
    }

    #[test]
    fn the_span_includes_a_prefix() {
        let text = "c = {\n    color = rgb { 51 102 153 }\n}\n";
        let found = find(text, "color");
        let line: Vec<char> = text.lines().nth(1).unwrap().chars().collect();
        let slice: String = line[found.range.start.col as usize..found.range.end.col as usize]
            .iter()
            .collect();
        assert_eq!(slice, "rgb { 51 102 153 }");
        assert_eq!(found.literal.convention.space, Space::RgbInt);
    }

    #[test]
    fn a_multi_line_literal_spans_both_lines() {
        let text = "c = {\n    color = {\n        51 102 153\n    }\n}\n";
        let found = find(text, "color");
        assert_eq!(found.range.start.line, 2);
        assert_eq!(found.range.end.line, 4);
        assert_eq!(found.literal.channels, [51.0, 102.0, 153.0]);
    }

    #[test]
    fn range_text_extracts_the_literal_for_the_presentation_step() {
        let text = "c = {\n    color = { 0.2 0.4 0.6 }\n}\n";
        let range = Range::new(Position::new(1, 12), Position::new(1, 27));
        let got = text_in_range(text, range, &PositionEncodingKind::UTF16);
        assert_eq!(got, "{ 0.2 0.4 0.6 }");
        assert_eq!(
            parse_literal(&got).unwrap().convention,
            Convention::new(Space::RgbFloat)
        );
    }

    #[test]
    fn range_text_joins_a_multi_line_literal() {
        let text = "c = {\n    color = {\n        51 102 153\n    }\n}\n";
        let range = Range::new(Position::new(1, 12), Position::new(3, 5));
        let got = text_in_range(text, range, &PositionEncodingKind::UTF16);
        assert_eq!(parse_literal(&got).unwrap().channels, [51.0, 102.0, 153.0]);
    }

    #[test]
    fn range_text_uses_the_negotiated_encoding() {
        // 😀 is two UTF-16 code units, so the literal starts at UTF-16 column 14
        let text = "c = {\n    😀 = { 1 0 0 }\n}\n";
        let utf16 = Range::new(Position::new(1, 9), Position::new(1, 20));
        assert_eq!(
            text_in_range(text, utf16, &PositionEncodingKind::UTF16),
            "{ 1 0 0 }"
        );
        let utf32 = Range::new(Position::new(1, 8), Position::new(1, 19));
        assert_eq!(
            text_in_range(text, utf32, &PositionEncodingKind::UTF32),
            "{ 1 0 0 }"
        );
    }

    fn colour_node(value: ValueType, min: i32) -> RuleType {
        RuleType::NodeRule {
            left: cwtools_rules::rules_types::NewField::SpecificField("color".into()),
            rules: [(
                RuleType::LeafValueRule {
                    right: cwtools_rules::rules_types::NewField::ValueField(value),
                },
                Options {
                    min,
                    max: min,
                    leafvalue: true,
                    ..Options::default()
                },
            )]
            .into(),
        }
    }

    #[test]
    fn the_post_processed_colour_shape_is_recognised() {
        assert!(is_colour_rule(&colour_node(
            ValueType::Float {
                min: -256.0,
                max: 256.0
            },
            3
        )));
        assert!(is_colour_rule(&colour_node(
            ValueType::Int { min: 0, max: 255 },
            3
        )));
    }

    #[test]
    fn a_plain_numeric_list_is_not_a_colour() {
        assert!(!is_colour_rule(&colour_node(
            ValueType::Int { min: 0, max: 255 },
            2
        )));
        assert!(!is_colour_rule(&RuleType::NodeRule {
            left: cwtools_rules::rules_types::NewField::SpecificField("color".into()),
            rules: [(
                RuleType::LeafRule {
                    left: cwtools_rules::rules_types::NewField::SpecificField("r".into()),
                    right: cwtools_rules::rules_types::NewField::ValueField(ValueType::Int {
                        min: 0,
                        max: 255
                    }),
                },
                Options::default(),
            )]
            .into(),
        }));
        assert!(!is_colour_rule(&RuleType::LeafValueRule {
            right: cwtools_rules::rules_types::NewField::ValueField(ValueType::Bool),
        }));
    }
}
