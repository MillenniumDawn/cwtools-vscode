//! Locale selection and the server's own user-visible strings.
//!
//! Leaf crate (depends on nothing) so `cwtools_cache`, `cwtools_error_codes`,
//! `cwtools_validation` and `cwtools_lsp` can all reach the active locale
//! without a dependency cycle.
//!
//! The locale is process-global and set once, from the `locale` the client
//! sends in `initialize` (`vscode-languageclient` fills it in from
//! `vscode.env.language`). Nothing sets it in the CLI, so batch runs stay
//! English and the corpus baselines don't move.
//!
//! Two string sets live in two places, each next to the code that owns it:
//! the diagnostic message templates are in `cwtools_error_codes`, beside the
//! English catalog they translate; everything the server says on its own
//! behalf — progress, command results, code-action titles, hover labels — is
//! here.
//!
//! # Examples
//!
//! ```
//! use cwtools_i18n::{Key, Locale};
//!
//! assert_eq!(Locale::from_tag("zh-CN"), Locale::ZhCn);
//! assert_eq!(Locale::from_tag("pt-br"), Locale::En);
//! assert_eq!(Key::ProgressCancelled.en(), "Cancelled.");
//! ```

use std::sync::atomic::{AtomicU8, Ordering};

mod locales;

/// The languages the server ships strings for. Anything else is [`Locale::En`],
/// which is also the compiled-in fallback for a key a locale hasn't translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Ar,
    De,
    Es,
    Fr,
    It,
    ZhCn,
    ZhTw,
}

impl Locale {
    /// Pick a locale from a BCP-47 tag. Matching is on the primary subtag, so
    /// `de-AT` and `fr-CA` land on the language we do have; Chinese splits on
    /// script/region because Simplified and Traditional are separate files.
    pub fn from_tag(tag: &str) -> Locale {
        let tag = tag.trim().to_ascii_lowercase();
        let mut parts = tag.split(['-', '_']);
        let primary = parts.next().unwrap_or_default();
        if primary == "zh" {
            // Hant/TW/HK are Traditional; everything else (including a bare
            // `zh`) is Simplified, which is what the overwhelming majority of
            // `zh` display languages mean.
            return match parts.next().unwrap_or_default() {
                "hant" | "tw" | "hk" | "mo" => Locale::ZhTw,
                _ => Locale::ZhCn,
            };
        }
        match primary {
            "ar" => Locale::Ar,
            "de" => Locale::De,
            "es" => Locale::Es,
            "fr" => Locale::Fr,
            "it" => Locale::It,
            _ => Locale::En,
        }
    }

    /// The tag this locale is keyed by, for anything that has to record which
    /// language a cached artefact was produced in.
    pub const fn tag(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Ar => "ar",
            Locale::De => "de",
            Locale::Es => "es",
            Locale::Fr => "fr",
            Locale::It => "it",
            Locale::ZhCn => "zh-cn",
            Locale::ZhTw => "zh-tw",
        }
    }

    const fn from_index(index: u8) -> Locale {
        match index {
            1 => Locale::Ar,
            2 => Locale::De,
            3 => Locale::Es,
            4 => Locale::Fr,
            5 => Locale::It,
            6 => Locale::ZhCn,
            7 => Locale::ZhTw,
            _ => Locale::En,
        }
    }

    const fn index(self) -> u8 {
        match self {
            Locale::En => 0,
            Locale::Ar => 1,
            Locale::De => 2,
            Locale::Es => 3,
            Locale::Fr => 4,
            Locale::It => 5,
            Locale::ZhCn => 6,
            Locale::ZhTw => 7,
        }
    }
}

static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Set the language every later `t`/`format` call answers in. Called once, from
/// the LSP's `initialize`; a second call wins, which is what a client that
/// restarts the server in a new display language wants.
pub fn set_locale(locale: Locale) {
    ACTIVE.store(locale.index(), Ordering::Relaxed);
}

/// The locale in force. [`Locale::En`] until something sets one.
pub fn locale() -> Locale {
    Locale::from_index(ACTIVE.load(Ordering::Relaxed))
}

macro_rules! keys {
    ($($(#[$doc:meta])* $variant:ident => $id:literal, $en:literal;)+) => {
        /// A string the server shows on its own behalf. The variant is what
        /// call sites name; `id` is what the per-locale tables are keyed by.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Key {
            $($(#[$doc])* $variant,)+
        }

        impl Key {
            /// Every key, for the tests that check the locale tables against it.
            pub const ALL: &'static [Key] = &[$(Key::$variant,)+];

            /// The table key, stable across wording changes to the English text.
            pub const fn id(self) -> &'static str {
                match self { $(Key::$variant => $id,)+ }
            }

            /// The English text, and the fallback for an untranslated key.
            pub const fn en(self) -> &'static str {
                match self { $(Key::$variant => $en,)+ }
            }
        }
    };
}

keys![
    // Workspace scan progress. Also the status-bar text, so these are the
    // strings a user sees most often.
    ProgressDiscover => "progress.discover", "Scanning workspace…";
    ProgressParse => "progress.parse", "Indexing workspace…";
    ProgressVanilla => "progress.vanilla", "Indexing base game…";
    ProgressLocalisation => "progress.localisation", "Building localisation index…";
    ProgressValidate => "progress.validate", "Validating workspace…";
    ProgressPublish => "progress.publish", "Publishing diagnostics…";
    ProgressCancelled => "progress.cancelled", "Cancelled.";

    // `workspace/executeCommand` results. The client toasts these verbatim, so
    // each half has to keep reporting honestly in translation: whether the
    // rules loaded, and whether the re-validation ran, queued or was cancelled.
    CommandRulesReloaded => "command.rulesReloaded", "Rules config reloaded; {}.";
    CommandNoRulesLoaded => "command.noRulesLoaded", "No rules loaded from {}; {}.";
    CommandNoRulesDirectory => "command.noRulesDirectory", "No rules directory configured; nothing to reload.";
    CommandCachesCleared => "command.cachesCleared", "Caches cleared ({} files); {}.";
    CommandCachesClearedWithErrors => "command.cachesClearedWithErrors", "Caches cleared ({} files) with {} error(s); {}. Failed: {}";
    CommandWorkspaceReindexed => "command.workspaceReindexed", "Workspace re-indexed.";
    CommandReindexInProgress => "command.reindexInProgress", "Re-index already in progress.";
    CommandReindexCancelled => "command.reindexCancelled", "Re-index cancelled.";
    CommandValidateWorkspace => "command.validateWorkspace", "Workspace validated.";

    // The second half of the two composed messages above.
    StatusRevalidated => "status.revalidated", "workspace re-validated";
    StatusRevalidationCancelled => "status.revalidationCancelled", "re-validation cancelled";
    StatusRevalidationQueued => "status.revalidationQueued", "re-validation queued behind the running scan";
    StatusRevalidationPending => "status.revalidationPending", "re-validation still pending (a scan is running)";
    StatusReindexed => "status.reindexed", "workspace re-indexed";
    StatusReindexPending => "status.reindexPending", "re-index still pending (another scan is running)";
    StatusReindexCancelledRebuilding => "status.reindexCancelledRebuilding", "re-index cancelled, rebuilding in the background";

    // Code-action titles, as they read in the lightbulb menu. `set_name` and
    // the CW code in the ignore action are script/catalog identifiers and stay
    // as they are.
    ActionFixAll => "action.fixAll", "Fix all ({} auto-fixable)";
    ActionIgnoreCode => "action.ignoreCode", "Ignore {} in this workspace";
    ActionCreateLoc => "action.createLocKey", "Create localisation key {}";
    ActionRemoveEmptyIf => "action.removeEmptyIf", "Remove empty if";
    ActionRemoveEmptyLimit => "action.removeEmptyLimit", "Remove empty limit";
    ActionRemoveRedundant => "action.removeRedundant", "Remove redundant {}";
    ActionRemoveRedundantDefault => "action.removeRedundantDefault", "Remove redundant default";
    ActionRemoveUnnecessaryQuotes => "action.removeUnnecessaryQuotes", "Remove unnecessary quotes";
    ActionRenameToSetName => "action.renameToSetName", "Rename to set_name";
    ActionDidYouMean => "action.didYouMean", "Did you mean '{}'?";

    // Hover labels. The values beside them are script identifiers and are left
    // alone, as are the ROOT/PREV/FROM rows, which name script keywords.
    HoverScope => "hover.scope", "Scope";
    HoverResolvesTo => "hover.resolvesTo", "Resolves to";
    HoverRequiredScopes => "hover.requiredScopes", "Required scopes";
    HoverLocalisation => "hover.localisation", "Localisation";
    HoverDescription => "hover.description", "Description";
];

/// The active locale's text for `key`, or the English text when that locale
/// hasn't translated it.
pub fn t(key: Key) -> &'static str {
    let table = table(locale());
    match table.binary_search_by_key(&key.id(), |(id, _)| *id) {
        Ok(i) => table[i].1,
        Err(_) => key.en(),
    }
}

/// [`t`] with each `{}` in the text replaced by the next `args` entry, in
/// order. Extra `{}` are left as they are, matching
/// `cwtools_error_codes::ErrorCode::format`, so a translation that grows a
/// placeholder shows the stray braces rather than swallowing text.
pub fn format(key: Key, args: &[&str]) -> String {
    format_template(t(key), args)
}

/// Substitute `args` into a `{}` template, positionally.
pub fn format_template(template: &str, args: &[&str]) -> String {
    let mut result = String::with_capacity(template.len());
    let mut it = args.iter();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'}') {
            chars.next();
            match it.next() {
                Some(arg) => result.push_str(arg),
                None => result.push_str("{}"),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// One locale's translated strings, sorted by key. Public so the tests that
/// check a table against [`Key`] can walk it.
pub fn table(locale: Locale) -> &'static [(&'static str, &'static str)] {
    match locale {
        Locale::En => &[],
        Locale::Ar => locales::ar::UI,
        Locale::De => locales::de::UI,
        Locale::Es => locales::es::UI,
        Locale::Fr => locales::fr::UI,
        Locale::It => locales::it::UI,
        Locale::ZhCn => locales::zh_cn::UI,
        Locale::ZhTw => locales::zh_tw::UI,
    }
}

/// Every locale that has a table, for tests and for anything enumerating them.
pub const TRANSLATED: &[Locale] = &[
    Locale::Ar,
    Locale::De,
    Locale::Es,
    Locale::Fr,
    Locale::It,
    Locale::ZhCn,
    Locale::ZhTw,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> usize {
        text.matches("{}").count()
    }

    #[test]
    fn tags_map_to_locales() {
        assert_eq!(Locale::from_tag("de"), Locale::De);
        assert_eq!(Locale::from_tag("de-AT"), Locale::De);
        assert_eq!(Locale::from_tag("zh-cn"), Locale::ZhCn);
        assert_eq!(Locale::from_tag("zh"), Locale::ZhCn);
        assert_eq!(Locale::from_tag("zh-Hant"), Locale::ZhTw);
        assert_eq!(Locale::from_tag("zh-TW"), Locale::ZhTw);
        assert_eq!(Locale::from_tag("ar-EG"), Locale::Ar);
        // A tag merely starting with the same letters is not a match.
        assert_eq!(Locale::from_tag("art"), Locale::En);
        assert_eq!(Locale::from_tag("pt-br"), Locale::En);
        assert_eq!(Locale::from_tag(""), Locale::En);
    }

    #[test]
    fn every_locale_round_trips_its_index() {
        for locale in TRANSLATED.iter().chain(std::iter::once(&Locale::En)) {
            assert_eq!(Locale::from_index(locale.index()), *locale);
        }
    }

    #[test]
    fn tables_are_sorted_and_unique() {
        for locale in TRANSLATED {
            let table = table(*locale);
            for pair in table.windows(2) {
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
    fn tables_key_only_real_keys() {
        let known: Vec<&str> = Key::ALL.iter().map(|k| k.id()).collect();
        for locale in TRANSLATED {
            for (id, _) in table(*locale) {
                assert!(
                    known.contains(id),
                    "{}: `{}` is not a Key id",
                    locale.tag(),
                    id
                );
            }
        }
    }

    // A translation that drops a `{}` loses whatever the server was naming
    // there; one that grows a `{}` renders literal braces. Both are silent at
    // runtime, so they're caught here instead.
    #[test]
    fn translations_keep_their_placeholders() {
        for locale in TRANSLATED {
            for (id, text) in table(*locale) {
                let key = Key::ALL
                    .iter()
                    .find(|k| k.id() == *id)
                    .expect("checked by tables_key_only_real_keys");
                assert_eq!(
                    placeholders(text),
                    placeholders(key.en()),
                    "{}: `{}` has the wrong number of placeholders",
                    locale.tag(),
                    id
                );
            }
        }
    }

    // The seven locales ship as complete sets, so a key present in six of them
    // and missing from the seventh is a dropped line, not a translator's
    // choice — and it is invisible at runtime, because the gap renders in
    // English like any deliberate omission. Relaxing this is how a genuinely
    // partial locale would land, and it should be a deliberate edit.
    #[test]
    fn every_locale_covers_the_same_keys() {
        let reference: Vec<&str> = table(Locale::De).iter().map(|(id, _)| *id).collect();
        for locale in TRANSLATED {
            let ids: Vec<&str> = table(*locale).iter().map(|(id, _)| *id).collect();
            assert_eq!(
                ids,
                reference,
                "{} does not cover the same keys as the other locales",
                locale.tag()
            );
        }
    }

    #[test]
    fn untranslated_keys_fall_back_to_english() {
        // En has no table at all, so every lookup takes the fallback path.
        assert_eq!(
            t(Key::ProgressCancelled),
            Key::ProgressCancelled.en(),
            "the default locale must answer in English"
        );
    }

    #[test]
    fn positional_arguments_substitute_in_order() {
        assert_eq!(format_template("{} of {}", &["a", "b"]), "a of b");
        assert_eq!(format_template("{} of {}", &["a"]), "a of {}");
        assert_eq!(format_template("no args", &["a"]), "no args");
    }

    // Paradox script is full of braces, and several messages quote it
    // (`{ always = ... }`, `NOT = { OR = { ... } }`). Only an empty pair is a
    // substitution point; everything else has to survive verbatim.
    #[test]
    fn braces_that_are_not_a_placeholder_survive() {
        assert_eq!(
            format_template("{} = { always = ... } is the default", &["fixed"]),
            "fixed = { always = ... } is the default"
        );
        assert_eq!(
            format_template("NOT = { OR = { } }", &[]),
            "NOT = { OR = { } }"
        );
    }
}
