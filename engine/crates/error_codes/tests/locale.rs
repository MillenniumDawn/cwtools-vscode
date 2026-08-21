//! End to end over a non-English locale. The locale is process-global, so this
//! lives in its own test binary: the unit tests in `src/` assert the English
//! wording and would see whatever this set.

use cwtools_error_codes::{
    CW100_MISSING_LOCALISATION, CW223_INCORRECT_NOT_USAGE_HOI4_MSG, CW240_UNEXPECTED_VALUE,
    CW271_VARIABLE_INT_ONLY, cw223_hoi4_message,
};

#[test]
fn diagnostics_come_back_in_the_active_locale() {
    cwtools_i18n::set_locale(cwtools_i18n::Locale::De);

    // Template and arguments both land, in the translated wording.
    assert_eq!(
        CW100_MISSING_LOCALISATION.format(&["my_key", "english"]),
        "Lokalisierungsschlüssel my_key ist für english nicht definiert"
    );
    // A code read as a bare template, not through `format`.
    assert_eq!(
        CW271_VARIABLE_INT_ONLY.message(),
        "Erwartet wurde eine ganze Zahl"
    );
    // CW223's second English message travels with it.
    assert_ne!(cw223_hoi4_message(), CW223_INCORRECT_NOT_USAGE_HOI4_MSG);
    assert!(cw223_hoi4_message().contains("NOT = { OR = { ... } }"));

    // A pass-through code has no text of its own; the message the emit site
    // built has to survive untouched.
    assert_eq!(
        CW240_UNEXPECTED_VALUE.format(&["expecting yes/no, got 3"]),
        "expecting yes/no, got 3"
    );

    // An unknown tag is English, not a panic or an empty string.
    cwtools_i18n::set_locale(cwtools_i18n::Locale::from_tag("pt-br"));
    assert_eq!(
        CW271_VARIABLE_INT_ONLY.message(),
        CW271_VARIABLE_INT_ONLY.message_template
    );
}
