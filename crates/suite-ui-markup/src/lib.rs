//! Generated bindings for Stockroom's `.slint` files, and the translation catalogs they bundle.

pub mod ui {
    #![allow(clippy::all, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/app_ui.rs"));
}

/// The bundled `.po` catalogs as (language, text), for strings the UI builds in Rust.
pub const CATALOGS: [(&str, &str); 4] = [
    ("de", include_str!("../translations/de/LC_MESSAGES/suite-ui-markup.po")),
    ("es", include_str!("../translations/es/LC_MESSAGES/suite-ui-markup.po")),
    ("fr", include_str!("../translations/fr/LC_MESSAGES/suite-ui-markup.po")),
    ("nl", include_str!("../translations/nl/LC_MESSAGES/suite-ui-markup.po")),
];

/// The template every catalog translates.
pub const TEMPLATE: &str = include_str!("../translations/suite-ui-markup.pot");
