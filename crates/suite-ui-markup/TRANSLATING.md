# Translating Stockroom

The window's text lives in one gettext catalog per language:

```
crates/suite-ui-markup/translations/
  suite-ui-markup.pot                     template: every string, regenerated from the code
  de/LC_MESSAGES/suite-ui-markup.po       German
  es/LC_MESSAGES/suite-ui-markup.po       Spanish
  fr/LC_MESSAGES/suite-ui-markup.po       French
  nl/LC_MESSAGES/suite-ui-markup.po       Dutch
```

The catalogs are compiled into the binary (Slint's bundled translations), so nothing is
installed next to it and gettext isn't needed at runtime. Strings built in Rust (`tr!` and
`trn!` in `suite-ui`) read the same catalogs. The languages are the union of the family's
(PLAN.md §6).

## Add a language

1. `mkdir -p crates/suite-ui-markup/translations/<code>/LC_MESSAGES` and copy the template
   there as `suite-ui-markup.po`.
2. Set `"Language: <code>\n"` and the right `Plural-Forms` in the header.
3. Translate every `msgstr`. Add the code and the language's own name to `LANGUAGES` in
   `crates/suite-ui/src/i18n.rs` and the catalog to `CATALOGS` in `crates/suite-ui-markup/src/lib.rs`.
4. Languages whose plural rule isn't `n != 1` (French: `n > 1`) also need `i18n::plural`
   taught their rule.

## Update after code changes

```sh
python3 crates/suite-ui-markup/tools/extract.py   # rewrites the .pot, msgmerges every .po, checks them
cargo test -p suite-ui i18n                        # fails if a catalog misses a string
```

## Rules for translators

- Keep placeholders exactly: `{}` (next value), `{0}` `{1}` (a given value, reorder freely),
  `{n}` (the count in plural messages).
- Product and store names stay as they are: Stockroom, Flatpak, Homebrew, winget, Microsoft
  Store, App Store, systemd, LaunchAgent, Task Scheduler (use the OS's own localized name
  where Windows has one, e.g. "Aufgabenplanung").
- Captions in capitals ("ABOUT", "PAST CHANGES") are small-caps headings; keep them in capitals.
- Buttons and segmented choices are short; keep them about as long as the English.
- Plain words over jargon: these messages are read by people who don't know what a
  checksum or a catalog is ("the app list").
- Use the informal address your language's desktop software uses (German: du, Spanish: tú,
  Dutch: je, French: vous).
