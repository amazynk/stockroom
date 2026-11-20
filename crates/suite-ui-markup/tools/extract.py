#!/usr/bin/env python3
"""Collects every `@tr(...)` in the markup and `tr!`/`trn!` in suite-ui into translations/suite-ui-markup.pot,
then merges it into each language's catalog with msgmerge (when installed).

Run from anywhere: python3 crates/suite-ui-markup/tools/extract.py
"""
import pathlib
import re
import shutil
import subprocess

CRATE = pathlib.Path(__file__).resolve().parent.parent
ROOT = CRATE.parent.parent
POT = CRATE / "translations" / "suite-ui-markup.pot"
STR = r'"((?:[^"\\]|\\.)*)"'
SLINT = re.compile(r'@tr\(\s*(?:' + STR + r'\s*=>\s*)?' + STR + r'(?:\s*\|\s*' + STR + r'\s*%)?', re.S)
RUST = re.compile(r'\btrn?!\(\s*' + STR + r'(?:\s*,\s*' + STR + r'\s*,)?', re.S)
RUST_CTX = re.compile(r'\btrc!\(\s*' + STR + r'\s*,\s*' + STR + r'\s*\)', re.S)


def unescape(s):
    return re.sub(r'\\(.)', lambda m: {"n": "\n", "t": "\t"}.get(m.group(1), m.group(1)), s)


def po_quote(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def collect():
    entries = {}
    sources = [(p, SLINT, True) for p in sorted((CRATE / "ui").rglob("*.slint"))]
    sources += [(p, RUST, False) for p in sorted((ROOT / "crates" / "suite-ui" / "src").rglob("*.rs"))
                if p.name != "i18n.rs"]
    for path, pattern, slint in sources:
        text = path.read_text()
        found = []
        for m in pattern.finditer(text):
            if slint:
                ctx, msgid, plural = m.groups()
                if ctx:
                    raise SystemExit(f"{path}: the markup has no default context; drop `{ctx} =>`")
            else:
                ctx, (msgid, plural) = None, m.groups()
            found.append((m.start(), ctx, msgid, plural))
        if not slint:
            found += [(m.start(), m.group(1), m.group(2), None) for m in RUST_CTX.finditer(text)]
        for start, ctx, msgid, plural in sorted(found):
            msgid, plural = unescape(msgid), plural and unescape(plural)
            line = text.count("\n", 0, start) + 1
            entry = entries.setdefault((ctx, msgid), {"plural": plural, "refs": []})
            entry["plural"] = entry["plural"] or plural
            entry["refs"].append(f"{path.relative_to(ROOT)}:{line}")
    return entries


def write_pot(entries):
    out = [
        '# Stockroom user interface. See crates/suite-ui-markup/TRANSLATING.md.',
        'msgid ""',
        'msgstr ""',
        '"Project-Id-Version: stockroom\\n"',
        '"Language-Team: none\\n"',
        '"Last-Translator: Stockroom contributors\\n"',
        '"PO-Revision-Date: 2026-11-15 00:00+0000\\n"',
        '"MIME-Version: 1.0\\n"',
        '"Content-Type: text/plain; charset=UTF-8\\n"',
        '"Content-Transfer-Encoding: 8bit\\n"',
        '"Plural-Forms: nplurals=2; plural=(n != 1);\\n"',
        "",
    ]
    for (ctx, msgid), e in entries.items():
        out.append("#: " + " ".join(e["refs"]))
        if ctx:
            out.append("msgctxt " + po_quote(ctx))
        out.append("msgid " + po_quote(msgid))
        if e["plural"]:
            out.append("msgid_plural " + po_quote(e["plural"]))
            out += ['msgstr[0] ""', 'msgstr[1] ""']
        else:
            out.append('msgstr ""')
        out.append("")
    POT.parent.mkdir(parents=True, exist_ok=True)
    POT.write_text("\n".join(out))


def main():
    entries = collect()
    write_pot(entries)
    print(f"{len(entries)} messages -> {POT.relative_to(ROOT)}")
    if not shutil.which("msgmerge"):
        return
    for po in sorted((CRATE / "translations").glob("*/LC_MESSAGES/suite-ui-markup.po")):
        subprocess.run(["msgmerge", "--quiet", "--update", "--backup=none", "--no-fuzzy-matching", str(po), str(POT)],
                       check=True)
        subprocess.run(["msgfmt", "--check", "--statistics", "-o", "/dev/null", str(po)], check=True)


if __name__ == "__main__":
    main()
