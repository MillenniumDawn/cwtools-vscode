//! One module per translated locale, each a `UI` table sorted by [`crate::Key`]
//! id so lookups can binary-search it. A key a locale hasn't translated is
//! simply absent, and falls back to the English text in `Key::en`.

pub(crate) mod ar;
pub(crate) mod de;
pub(crate) mod es;
pub(crate) mod fr;
pub(crate) mod it;
pub(crate) mod zh_cn;
pub(crate) mod zh_tw;
