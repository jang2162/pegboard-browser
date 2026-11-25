#!/usr/bin/env python3
"""rebrand-strings.py <src>: the product name "Chromium" becomes "Pegboard" in the strings.

Targets (design 4.7, B15): chrome/app/chromium_strings.grd,
components/components_chromium_strings.grd and every .xtb their <translations> name.

- In each <message> body, "Chromium" becomes "Pegboard" except inside PROTECTED (the
  authors, ChromiumOS, addresses, the Chromium project link of the open source notice).
  Messages named in EXCLUDED_MESSAGES are left alone.
- A translation is tied to its English message by grit's fingerprint of the message text, so
  changing the text changes the message ID. For every changed message the old and new IDs are
  computed with the tree's own grit (<src>/tools/grit) and the .xtb translations with the old
  ID get the new ID and the same name change (plus Korean particle fixes, KOREAN_PARTICLES).
- Afterwards every .xtb must still translate as many messages of its .grd as before; if not,
  the script fails (a mismatch would silently turn those strings back to English).
- Running it again changes nothing.

Prints the number of changed and excluded messages and the translations changed per file.
"""

import os
import re
import sys

PRODUCT = "Chromium"
NEW_PRODUCT = "Pegboard"

TARGETS = (
    "chrome/app/chromium_strings.grd",
    "components/components_chromium_strings.grd",
)

# Text that keeps "Chromium": the copyright holder, the OS name, addresses, and the link to the
# Chromium project in the open source notice ("Pegboard is made possible by the Chromium open
# source project"). The link span is found by its placeholders, so it works in every language.
PROTECTED = re.compile(
    r"The Chromium Authors"
    r"|Chromium\s*OS"
    r"|https?://[^\s<\"]*"
    r"|[\w.-]*chromium\.org[^\s<\"]*"
    r'|<ph name="BEGIN_LINK_CHROMIUM".*?<ph name="END_LINK_CHROMIUM"'
    r"|<[^>]*>",  # markup itself (placeholder names, tags)
    re.DOTALL | re.IGNORECASE,
)

# Messages left as they are, by name.
EXCLUDED_MESSAGES = frozenset()

# "Chromium" ends in a consonant, "Pegboard" (페그보드) in a vowel: the particle after it changes.
KOREAN_PARTICLES = (("으로", "로"), ("이", "가"), ("을", "를"), ("은", "는"), ("과", "와"))
_KO_AFTER = re.compile(
    "(" + "|".join(old for old, _ in KOREAN_PARTICLES) + ")(?![가-힣])"
)

_MESSAGE = re.compile(r"(<message\b)([^>]*)(>)(.*?)(</message>)", re.DOTALL)
_NAME = re.compile(r'\bname\s*=\s*"([^"]+)"')
_TRANSLATION = re.compile(
    r'(<translation id=")(\d+)("[^>]*>)(.*?)(</translation>)(\n?)', re.DOTALL
)
_XTB_FILE = re.compile(r'<file path="([^"]+)" lang="([^"]+)"')
_PART = re.compile(r'<part file="([^"]+)"')


def rename(text, lang=None):
    """Returns (new text, names changed, names kept) for one message body or translation."""
    out = []
    changed = kept = 0
    pos = 0
    for m in list(PROTECTED.finditer(text)) + [None]:
        end = m.start() if m else len(text)
        seg = text[pos:end]
        pieces = seg.split(PRODUCT)
        changed += len(pieces) - 1
        if lang == "ko":
            pieces = [pieces[0]] + [
                _KO_AFTER.sub(lambda p: dict(KOREAN_PARTICLES)[p.group(1)], s, count=1)
                for s in pieces[1:]
            ]
        out.append(NEW_PRODUCT.join(pieces))
        if m:
            out.append(m.group(0))
            kept += m.group(0).count(PRODUCT)
            pos = m.end()
    return "".join(out), changed, kept


def message_ids(grit_dir, grd_path):
    """[(name, id)] of every <message> (parts included) in document order, as grit computes them."""
    if grit_dir not in sys.path:
        sys.path.insert(0, grit_dir)
    from grit import grd_reader
    from grit.node import message

    root = grd_reader.Parse(grd_path, skip_validation_checks=True)
    return [
        (n.attrs["name"], n.GetCliques()[0].GetMessage().GetId())
        for n in root.GetChildrenOfType(message.MessageNode)
    ]


def grd_files(path):
    """The .grd and the .grdp parts it includes (recursively)."""
    with open(path, encoding="utf-8", newline="") as f:
        text = f.read()
    files = [path]
    for part in _PART.findall(text):
        files += grd_files(os.path.join(os.path.dirname(path), part))
    return files


def xtb_ids(grit_dir, path):
    """The set of translation IDs in an .xtb, read with grit's own reader."""
    if grit_dir not in sys.path:
        sys.path.insert(0, grit_dir)
    from grit import xtb_reader

    ids = set()
    with open(path, "rb") as f:
        xtb_reader.Parse(f, lambda msg_id, parts: ids.add(msg_id))
    return ids


def rebrand_grd(src, rel):
    grit_dir = os.path.join(src, "tools", "grit")
    grd_path = os.path.join(src, rel)
    with open(grd_path, encoding="utf-8", newline="") as f:
        grd_text = f.read()
    old_ids = message_ids(grit_dir, grd_path)
    xtbs = _XTB_FILE.findall(grd_text)
    translated_before = {}
    for p, _ in xtbs:
        ids = xtb_ids(grit_dir, os.path.join(os.path.dirname(grd_path), p))
        translated_before[p] = {name for name, i in old_ids if i in ids}

    stats = {"changed": 0, "excluded": 0}

    def one(m):
        name = _NAME.search(m.group(2)).group(1)
        body = m.group(4)
        if PRODUCT not in body:
            return m.group(0)
        if name in EXCLUDED_MESSAGES:
            stats["excluded"] += 1
            return m.group(0)
        new_body, changed, _ = rename(body)
        if not changed:
            stats["excluded"] += 1
            return m.group(0)
        stats["changed"] += 1
        return m.group(1) + m.group(2) + m.group(3) + new_body + m.group(5)

    for path in grd_files(grd_path):
        with open(path, encoding="utf-8", newline="") as f:
            old = f.read()
        new = _MESSAGE.sub(one, old)
        if new != old:
            with open(path, "w", encoding="utf-8", newline="") as f:
                f.write(new)

    new_ids = message_ids(grit_dir, grd_path)
    if [n for n, _ in old_ids] != [n for n, _ in new_ids]:
        sys.exit(f"error: {rel}: the messages changed order or number")
    id_map = {}
    for (name, o), (_, n) in zip(old_ids, new_ids):
        if o != n:
            if id_map.get(o, n) != n:
                sys.exit(f"error: {rel}: message ID {o} would become two IDs")
            id_map[o] = n
    still_used = {i for _, i in new_ids}

    per_file = []
    untouched = 0
    for p, lang in xtbs:
        path = os.path.join(os.path.dirname(grd_path), p)
        with open(path, encoding="utf-8", newline="") as f:
            text = f.read()
        present = set(m.group(2) for m in _TRANSLATION.finditer(text))
        count = 0

        def tr(m):
            nonlocal count, untouched
            o = m.group(2)
            if o not in id_map:
                return m.group(0)
            n = id_map[o]
            keep_old = o in still_used  # another message still has the old text
            if n in present:  # the new text already has a translation (same English text)
                return m.group(0) if keep_old else ""
            present.add(n)
            body, changed, _ = rename(m.group(4), lang)
            if not changed:
                untouched += 1
            count += 1
            rewritten = m.group(1) + n + m.group(3) + body + m.group(5) + m.group(6)
            return (m.group(0) + rewritten) if keep_old else rewritten

        new_text = _TRANSLATION.sub(tr, text)
        if new_text != text:
            with open(path, "w", encoding="utf-8", newline="") as f:
                f.write(new_text)
        ids = xtb_ids(grit_dir, path)
        lost = translated_before[p] - {name for name, i in new_ids if i in ids}
        if lost:
            sys.exit(f"error: {p}: lost the translation of {', '.join(sorted(lost)[:5])}")
        per_file.append(f"{lang} {count}")

    print(
        f"{rel}: {stats['changed']} messages changed, {stats['excluded']} excluded "
        f"(kept \"{PRODUCT}\"), {len(id_map)} message IDs rewritten"
    )
    print(f"  translations changed per file: {', '.join(per_file)}")
    if untouched:
        print(f"  {untouched} translations got the new ID but had no \"{PRODUCT}\" to change")
    return stats["changed"]


def main(argv):
    if len(argv) != 2 or not os.path.isdir(argv[1]):
        sys.exit("usage: rebrand-strings.py <chromium-src-dir>")
    src = os.path.abspath(argv[1])
    for rel in TARGETS:
        rebrand_grd(src, rel)


if __name__ == "__main__":
    main(sys.argv)
