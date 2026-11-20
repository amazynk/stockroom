//! Strings built in Rust, translated from the same `.po` catalogs Slint bundles, in the language
//! Slint shows. `tr!` and `trn!` take Slint's placeholders: `{}`, `{0}`, and `{n}` for the count.

use std::{cell::RefCell, collections::HashMap, fmt::Display};

/// Codes the UI can show, after "System": English is the source language. The union of the
/// family's languages (PLAN.md §6).
pub(crate) const LANGUAGES: [(&str, &str); 5] =
    [("en", "English"), ("de", "Deutsch"), ("es", "Español"), ("fr", "Français"), ("nl", "Nederlands")];

thread_local! {
    /// Per thread: strings are built on the UI thread, and tests on different threads can
    /// use different languages side by side.
    static CURRENT: RefCell<Option<Catalog>> = const { RefCell::new(None) };
}

#[derive(Debug, Default)]
struct Catalog {
    code: String,
    /// msgid → msgstr, or the plural forms for a msgid with a plural.
    messages: HashMap<String, Vec<String>>,
}

/// The language to show: `choice` if set, else the system's, else English.
pub(crate) fn resolve(choice: Option<&str>) -> &'static str {
    let wanted = choice.map(str::to_owned).or_else(sys_locale::get_locale).unwrap_or_default();
    let base = wanted.split(['-', '_', '.', '@']).next().unwrap_or_default().to_ascii_lowercase();
    LANGUAGES.iter().find(|l| l.0 == base).map_or("en", |l| l.0)
}

/// Switches Rust strings and the markup to `code`.
pub(crate) fn select(code: &str) {
    use_catalog(code);
    select_markup(code);
}

/// Switches Rust strings only; the markup can't switch before its first window exists.
pub(crate) fn use_catalog(code: &str) {
    let catalog = suite_ui_markup::CATALOGS
        .iter()
        .find(|c| c.0 == code)
        .map(|c| Catalog { code: code.to_owned(), messages: parse(c.1) });
    CURRENT.set(catalog);
}

/// Overrides the language Slint picked from the system locale; call after creating a window.
pub(crate) fn select_markup(code: &str) {
    if let Err(e) = slint::select_bundled_translation(code) {
        tracing::warn!(language = code, error = %e, "could not switch the markup's language");
    }
}

/// The language Rust strings are in now.
pub(crate) fn current() -> String {
    CURRENT.with_borrow(|c| c.as_ref().map(|c| c.code.clone())).unwrap_or_else(|| "en".into())
}

pub(crate) fn text(msgid: &str) -> String {
    lookup(msgid, 0).unwrap_or_else(|| msgid.to_owned())
}

pub(crate) fn plural(one: &str, many: &str, n: i64) -> String {
    // French counts 0 and 1 as singular; the other bundled languages use `n != 1`.
    let form = if current() == "fr" { usize::from(n > 1) } else { usize::from(n != 1) };
    lookup(one, form).unwrap_or_else(|| if form == 0 { one } else { many }.to_owned())
}

fn lookup(msgid: &str, form: usize) -> Option<String> {
    CURRENT.with_borrow(|current| {
        let forms = current.as_ref()?.messages.get(msgid)?;
        forms.get(form).or_else(|| forms.first()).filter(|s| !s.is_empty()).cloned()
    })
}

/// The decimal separator of the language shown.
pub(crate) fn decimal() -> char {
    if current() == "en" { '.' } else { ',' }
}

/// The index of the language `choice` names in [`LANGUAGES`], plus one: 0 is "System".
pub(crate) fn index_of(choice: Option<&str>) -> usize {
    choice.and_then(|c| LANGUAGES.iter().position(|l| l.0 == c)).map_or(0, |i| i + 1)
}

/// `{}` takes the next argument, `{0}`… a given one, `{n}` the count; `{{` and `}}` escape.
pub(crate) fn format(template: &str, args: &[&dyn Display], n: Option<i64>) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            out.push_str(&rest[..1]);
            rest = &rest[2..];
            continue;
        }
        let Some(end) = rest.find('}').filter(|_| rest.starts_with('{')) else {
            out.push_str(&rest[..1]);
            rest = &rest[1..];
            continue;
        };
        let key = &rest[1..end];
        rest = &rest[end + 1..];
        let value = match key {
            "" => {
                next += 1;
                args.get(next - 1).map(ToString::to_string)
            }
            "n" => n.map(|n| n.to_string()),
            k => k.parse::<usize>().ok().and_then(|i| args.get(i)).map(ToString::to_string),
        };
        out.push_str(&value.unwrap_or_default());
    }
    out.push_str(rest);
    out
}

/// A user-visible string from Rust, in the shown language.
macro_rules! tr {
    ($s:literal) => {
        $crate::i18n::text($s)
    };
    ($s:literal, $($arg:expr),+ $(,)?) => {
        $crate::i18n::format(&$crate::i18n::text($s), &[$(&$arg as &dyn std::fmt::Display),+], None)
    };
}

/// A string with a count: `trn!("{n} item", "{n} items", n)`, plus `{}` arguments if any.
macro_rules! trn {
    ($one:literal, $many:literal, $n:expr $(, $arg:expr)* $(,)?) => {{
        let n = i64::try_from($n).unwrap_or(i64::MAX);
        $crate::i18n::format(&$crate::i18n::plural($one, $many, n), &[$(&$arg as &dyn std::fmt::Display),*], Some(n))
    }};
}

pub(crate) use tr;
pub(crate) use trn;

fn parse(po: &str) -> HashMap<String, Vec<String>> {
    parse_all(po, false)
}

/// The subset of the `.po` format our catalogs use: `msgctxt`, `msgid`, `msgid_plural`, `msgstr`, `msgstr[i]`.
/// Untranslated entries are dropped unless `keep_empty`.
fn parse_all(po: &str, keep_empty: bool) -> HashMap<String, Vec<String>> {
    #[derive(PartialEq)]
    enum Field {
        Id,
        Plural,
        Str(usize),
        Other,
    }
    let mut out = HashMap::new();
    let (mut id, mut forms, mut field) = (String::new(), Vec::<String>::new(), Field::Other);
    let mut context: Option<String> = None;
    let mut flush = |id: &mut String, forms: &mut Vec<String>| {
        if !id.is_empty() && (keep_empty || forms.iter().any(|f| !f.is_empty())) {
            out.insert(std::mem::take(id), std::mem::take(forms));
        }
        id.clear();
        forms.clear();
    };
    for line in po.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once(' ') {
            Some((k, v)) if !line.starts_with('"') => (k, v),
            _ => ("", line),
        };
        let value = unquote(value);
        match key {
            "msgid" => {
                if context.is_none() {
                    flush(&mut id, &mut forms);
                }
                field = Field::Id;
                id = context.take().map_or_default(|c| c + "\u{4}");
                id.push_str(&value);
            }
            "msgid_plural" => field = Field::Plural,
            "msgctxt" => {
                flush(&mut id, &mut forms);
                field = Field::Other;
                context = Some(value);
            }
            "" => match field {
                Field::Id => id.push_str(&value),
                Field::Str(i) => {
                    if let Some(f) = forms.get_mut(i) {
                        f.push_str(&value);
                    }
                }
                Field::Plural | Field::Other => {}
            },
            k => {
                let i =
                    k.strip_prefix("msgstr[").and_then(|r| r.strip_suffix(']')).and_then(|i| i.parse().ok());
                let i = if k == "msgstr" { Some(0) } else { i };
                if let Some(i) = i {
                    if forms.len() <= i {
                        forms.resize(i + 1, String::new());
                    }
                    forms[i] = value;
                    field = Field::Str(i);
                }
            }
        }
    }
    flush(&mut id, &mut forms);
    out
}

fn unquote(s: &str) -> String {
    let inner = s.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or_default();
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PO: &str = r#"
msgid ""
msgstr ""
"Plural-Forms: nplurals=2; plural=(n != 1);\n"

#: a.slint
msgid "Removed {}"
msgstr "{} entfernt"

msgid "{n} item"
msgid_plural "{n} items"
msgstr[0] "{n} Eintrag"
msgstr[1] "{n} Einträge"

msgid "Untranslated"
msgstr ""

msgctxt "filter"
msgid "Done"
msgstr "Erledigt"

msgid "Long "
"line"
msgstr "Lange "
"Zeile \"zitiert\""
"#;

    #[test]
    fn parses_singular_plural_and_continuations() {
        let m = parse(PO);
        assert_eq!(m["Removed {}"], ["{} entfernt"]);
        assert_eq!(m["{n} item"], ["{n} Eintrag", "{n} Einträge"]);
        assert_eq!(m["Long line"], ["Lange Zeile \"zitiert\""]);
        assert!(!m.contains_key("Untranslated"));
        assert_eq!(m["filter\u{4}Done"], ["Erledigt"]);
        assert!(!m.contains_key(""));
    }

    #[test]
    fn formats_placeholders() {
        assert_eq!(format("{} of {}", &[&1, &"b"], None), "1 of b");
        assert_eq!(format("{1} {0}", &[&"a", &"b"], None), "b a");
        assert_eq!(format("{n} items {{x}}", &[], Some(3)), "3 items {x}");
        assert_eq!(format("unclosed { brace", &[], None), "unclosed { brace");
    }

    #[test]
    fn resolves_locales() {
        assert_eq!(resolve(Some("de_DE.UTF-8")), "de");
        assert_eq!(resolve(Some("es-MX")), "es");
        assert_eq!(resolve(Some("fr_CA")), "fr");
        assert_eq!(resolve(Some("nl-BE")), "nl");
        assert_eq!(resolve(Some("pt")), "en");
        assert_eq!(index_of(None), 0);
        assert_eq!(index_of(Some("nl")), 5);
    }

    fn shipped_codes() -> Vec<&'static str> {
        suite_ui_markup::CATALOGS.iter().map(|c| c.0).collect()
    }

    #[test]
    fn french_plurals_count_zero_as_one() {
        use_catalog("fr");
        assert_eq!(plural("{n} app", "{n} apps", 0), "{n} app");
        use_catalog("de");
        assert_eq!(plural("{n} app", "{n} apps", 0), "{n} apps");
        use_catalog("en");
    }

    #[test]
    fn shipped_catalogs_are_complete() {
        let template = parse_all(suite_ui_markup::TEMPLATE, true);
        assert!(template.len() > 150, "{}", template.len());
        assert_eq!(shipped_codes(), ["de", "es", "fr", "nl"]);
        for (code, po) in suite_ui_markup::CATALOGS {
            let m = parse(po);
            let mut missing: Vec<_> = template.keys().filter(|id| !m.contains_key(*id)).collect();
            missing.sort();
            assert!(missing.is_empty(), "{code} lacks {missing:?}");
            for (id, forms) in &m {
                let wanted = template.get(id).map_or(0, Vec::len);
                assert_eq!(forms.len(), wanted, "{code}: plural forms of {id:?}");
            }
        }
    }
}
