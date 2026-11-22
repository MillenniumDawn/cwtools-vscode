use cwtools_validation::ValidationError;

use super::common::{Opts, validate};

// Regression: alternative leafvalue rules in one block must be counted
// independently (mirrors F# RuleValidationService.checkCardinality, which uses
// Seq.sumBy per rule). A value matching an earlier (permissive) alternative
// must not "starve" a later one. Here `destroyer`/`cruiser` are members of
// enum[ship_units], so the enum rule's count is 2 and it must NOT report
// "appears 0 time(s)".
#[test]
fn leafvalue_alternatives_counted_independently() {
    let cwt = r#"
ship_name = {
    type = scalar
    ## cardinality = 0..1
    ship_types = {
        ## cardinality = ~1..inf
        <unit.ship_unit>
        ## cardinality = ~1..inf
        enum[ship_units]
    }
}

types = {
    type[ship_name] = {
        path = "game/common/units/names_ships"
    }
    type[unit] = {
        path = "game/common/units"
        subtype[ship_unit] = {}
    }
}

enums = {
    enum[ship_units] = {
        destroyer cruiser
    }
}
"#;

    let script = r#"
ship_name = {
    type = ship
    ship_types = {
        destroyer
        cruiser
    }
}
"#;
    let errors = validate(cwt, script, &Opts::DEFAULT);
    let enum_card_fp = errors
        .iter()
        .any(|e| e.message.contains("Enum(\"ship_units\")") && e.message.contains("appears 0"));
    assert!(
        !enum_card_fp,
        "false positive: enum[ship_units] reported 0 despite members present. Errors: {:?}",
        errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
}

fn colour_errors(form: &str, values: &str) -> Vec<ValidationError> {
    let cwt = format!(
        r#"
types = {{
    type[thing] = {{ path = "game/common/things" }}
}}
thing = {{
    color = colour[{form}]
}}
"#
    );
    let script = format!("thing = {{ color = {{ {values} }} }}\n");
    validate(&cwt, &script, &Opts::DEFAULT)
}

fn has_cw242(errors: &[ValidationError], expected: &str) -> bool {
    errors
        .iter()
        .any(|e| e.code == Some("CW242") && e.message.contains(expected))
}

#[test]
fn default_colour_integer_whole_form_passes() {
    let errors = colour_errors("auto", "255 0 0");
    assert!(
        errors.is_empty(),
        "valid integer rgb triplet must pass: {errors:?}"
    );
}

#[test]
fn default_colour_float_whole_form_passes() {
    let errors = colour_errors("auto", "1.0 2.0 0.0");
    assert!(
        errors.is_empty(),
        "valid hsv float triplet must pass: {errors:?}"
    );
}

#[test]
fn default_colour_fractional_hsv_triplet_passes() {
    let errors = colour_errors("auto", "0.5 0.25 1.5");
    assert!(
        errors.is_empty(),
        "valid fractional hsv triplet must pass: {errors:?}"
    );
}

#[test]
fn default_colour_optional_alpha_quad_passes() {
    for values in ["255 0 0 128", "0.5 0.25 1.5 2.0"] {
        let errors = colour_errors("auto", values);
        assert!(errors.is_empty(), "{values} must pass: {errors:?}");
    }
}

#[test]
fn default_colour_wrong_value_count_fails() {
    let empty = colour_errors("auto", "");
    assert!(
        has_cw242(&empty, "expected at least"),
        "0 channels: {empty:?}"
    );
    let two = colour_errors("auto", "255 0");
    assert!(has_cw242(&two, "expected at least"), "2 channels: {two:?}");
    let five = colour_errors("auto", "255 0 0 128 64");
    assert!(
        has_cw242(&five, "expected at most"),
        "5 integer channels: {five:?}"
    );
    let mixed_five = colour_errors("auto", "255 128 0 0.5 1.5");
    assert!(
        mixed_five
            .iter()
            .any(|e| e.message.contains("Unexpected bare value")),
        "5 mixed channels: {mixed_five:?}"
    );
}

#[test]
fn default_colour_out_of_range_channel_fails() {
    let rgb_over = colour_errors("auto", "300 0 0");
    assert!(
        has_cw242(&rgb_over, "expected at least"),
        "300 out of rgb range: {rgb_over:?}"
    );
    let hsv_over = colour_errors("auto", "3.0 0.0 0.0");
    assert!(
        has_cw242(&hsv_over, "expected at least"),
        "3.0 out of hsv range: {hsv_over:?}"
    );
}

#[test]
fn default_colour_mixed_forms_fail() {
    let mixed = colour_errors("auto", "255 0.5 0");
    assert!(
        has_cw242(&mixed, "expected at least"),
        "mixed rgb/hsv channels must fail: {mixed:?}"
    );
}

#[test]
fn default_colour_rgb_range_floats_stay_invalid() {
    let whole_floats = colour_errors("auto", "255.0 0.0 0.0");
    assert!(
        has_cw242(&whole_floats, "expected at least"),
        "rgb-range floats must keep failing: {whole_floats:?}"
    );
}

#[test]
fn explicit_rgb_and_hsv_channel_types_unchanged() {
    let rgb_ok = colour_errors("rgb", "255 0 0 128");
    assert!(rgb_ok.is_empty(), "explicit rgb ints must pass: {rgb_ok:?}");
    let rgb_float = colour_errors("rgb", "255 0.5 0");
    assert!(
        has_cw242(&rgb_float, "expected at least"),
        "explicit rgb must reject the 0.5 channel: {rgb_float:?}"
    );
    let hsv_ok = colour_errors("hsv", "0.5 0.25 1.5");
    assert!(
        hsv_ok.is_empty(),
        "explicit hsv floats must pass: {hsv_ok:?}"
    );
    let hsv_int = colour_errors("hsv", "255 0 0");
    assert!(
        has_cw242(&hsv_int, "expected at least"),
        "explicit hsv must reject the 255 channel: {hsv_int:?}"
    );
}
