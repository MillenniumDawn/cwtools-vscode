//! Translated diagnostic message templates, one module per locale.
//!
//! They live beside the English catalog rather than in `cwtools_i18n` so a code
//! and its translations move together, and so the parity test below can see
//! both halves. `cwtools_i18n` owns the locale itself and the strings the
//! server says on its own behalf.
//!
//! Each table is keyed by the `CWxxx` id, sorted so lookups can binary-search
//! it. A code a locale hasn't translated is absent and falls back to the
//! English `message_template`. `CW223.hoi4` is the one non-code key: HOI4 has
//! no `NOR`/`NAND`, so CW223 has a second English message there
//! ([`crate::CW223_INCORRECT_NOT_USAGE_HOI4_MSG`]) that needs translating too.

use cwtools_i18n::Locale;

mod ar;
mod de;
mod es;
mod fr;
mod it;
mod zh_cn;
mod zh_tw;

/// The active locale's template for `id`, or `None` when that locale hasn't
/// translated it (and for [`Locale::En`], which is the catalog itself).
pub(crate) fn template(id: &str) -> Option<&'static str> {
    let table = table(cwtools_i18n::locale());
    table
        .binary_search_by_key(&id, |(code, _)| *code)
        .ok()
        .map(|i| table[i].1)
}

/// One locale's templates. Public to the crate so the tests can walk them.
pub(crate) fn table(locale: Locale) -> &'static [(&'static str, &'static str)] {
    match locale {
        Locale::En => &[],
        Locale::Ar => ar::TEMPLATES,
        Locale::De => de::TEMPLATES,
        Locale::Es => es::TEMPLATES,
        Locale::Fr => fr::TEMPLATES,
        Locale::It => it::TEMPLATES,
        Locale::ZhCn => zh_cn::TEMPLATES,
        Locale::ZhTw => zh_tw::TEMPLATES,
    }
}

/// The key the HOI4 variant of CW223 is translated under.
pub(crate) const CW223_HOI4_KEY: &str = "CW223.hoi4";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CATALOG;

    fn placeholders(text: &str) -> usize {
        text.matches("{}").count()
    }

    fn english(id: &str) -> Option<&'static str> {
        if id == CW223_HOI4_KEY {
            return Some(crate::CW223_INCORRECT_NOT_USAGE_HOI4_MSG);
        }
        CATALOG
            .iter()
            .find(|(_, code)| code.id == id)
            .map(|(_, code)| code.message_template)
    }

    #[test]
    fn tables_are_sorted_and_unique() {
        for locale in cwtools_i18n::TRANSLATED {
            for pair in table(*locale).windows(2) {
                assert!(
                    pair[0].0 < pair[1].0,
                    "{}: {} and {} are out of order or duplicated",
                    locale.tag(),
                    pair[0].0,
                    pair[1].0
                );
            }
        }
    }

    #[test]
    fn tables_only_name_real_codes() {
        for locale in cwtools_i18n::TRANSLATED {
            for (id, _) in table(*locale) {
                assert!(
                    english(id).is_some(),
                    "{}: `{}` is not a catalog code",
                    locale.tag(),
                    id
                );
            }
        }
    }

    // A translation that drops a `{}` loses whatever the diagnostic was naming
    // there, and one that grows a `{}` renders literal braces. Both are silent
    // at runtime, so they are caught here.
    #[test]
    fn translations_keep_their_placeholders() {
        for locale in cwtools_i18n::TRANSLATED {
            for (id, text) in table(*locale) {
                let en = english(id).expect("checked by tables_only_name_real_codes");
                assert_eq!(
                    placeholders(text),
                    placeholders(en),
                    "{}: {} has the wrong number of placeholders",
                    locale.tag(),
                    id
                );
            }
        }
    }

    // The seven locales ship as complete sets, so a code present in six of them
    // and missing from the seventh is a dropped line, not a translator's
    // choice — and it is invisible at runtime, because the gap renders in
    // English like any deliberate omission.
    #[test]
    fn every_locale_covers_the_same_codes() {
        let reference: Vec<&str> = table(Locale::De).iter().map(|(id, _)| *id).collect();
        for locale in cwtools_i18n::TRANSLATED {
            let ids: Vec<&str> = table(*locale).iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids,
                reference,
                "{} does not cover the same codes as the other locales",
                locale.tag()
            );
        }
    }

    // A code whose template is a bare `{}` carries no English text of its own —
    // the message is built at the emit site — so translating it does nothing
    // but add a table entry that can drift.
    #[test]
    fn pass_through_codes_are_not_translated() {
        for locale in cwtools_i18n::TRANSLATED {
            for (id, _) in table(*locale) {
                assert_ne!(
                    english(id),
                    Some("{}"),
                    "{}: {} has no English text to translate",
                    locale.tag(),
                    id
                );
            }
        }
    }
}
