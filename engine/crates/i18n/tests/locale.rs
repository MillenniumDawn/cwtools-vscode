//! The lookup path over a real locale. `set_locale` is process-global, so this
//! runs in its own test binary rather than beside the unit tests, which assert
//! the English fallback.

use cwtools_i18n::{Key, Locale, format, locale, set_locale, t};

#[test]
fn strings_come_back_in_the_active_locale() {
    set_locale(Locale::ZhCn);
    assert_eq!(locale(), Locale::ZhCn);
    assert_eq!(t(Key::ProgressCancelled), "已取消。");
    assert_eq!(
        format(Key::ActionIgnoreCode, &["CW100"]),
        "在此工作区中忽略 CW100"
    );

    // Traditional and Simplified are separate tables, not one with a fallback.
    set_locale(Locale::ZhTw);
    assert_eq!(t(Key::ProgressDiscover), "正在掃描工作區…");
    assert_ne!(t(Key::ProgressDiscover), "正在扫描工作区…");

    // Back to a locale with no table: every key answers in English.
    set_locale(Locale::En);
    for key in Key::ALL {
        assert_eq!(t(*key), key.en(), "{} should fall back", key.id());
    }
}
