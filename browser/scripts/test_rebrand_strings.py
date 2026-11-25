#!/usr/bin/env python3
"""Tests rebrand-strings.py on small .grd/.grdp/.xtb files with the tree's own grit.

Usage: python3 browser/scripts/test_rebrand_strings.py --src <chromium-src-dir>

The fake tree has both target .grd files (one with a .grdp part) and Korean and British
English translations. Their IDs come from grit. The expected IDs after the change come from
a second, hand-written .grd with the expected English text, also read by grit, so the check
does not reuse the script's own ID mapping.
"""

import argparse
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "rebrand-strings.py")
SRC = None

GRD = """<?xml version="1.0" encoding="UTF-8"?>
<grit base_dir="." latest_public_release="0" current_release="1" source_lang_id="en">
  <outputs>
    <output filename="grit/test_strings.h" type="rc_header">
      <emit emit_type='prepend'></emit>
    </output>
  </outputs>
  <translations>
{files}
  </translations>
  <release seq="1" allow_pseudo="false">
    <messages fallback_to_english="true">
{messages}
    </messages>
  </release>
</grit>
"""

GRDP = """<?xml version="1.0" encoding="utf-8"?>
<grit-part>
{messages}
</grit-part>
"""

XTB = """<?xml version="1.0" ?>
<!DOCTYPE translationbundle>
<translationbundle lang="{lang}">
{translations}
</translationbundle>
"""

# name: (English before, English after, {lang: (translation before, translation after)})
CHROME = {
    "IDS_ABOUT": (
        "About Chromium",
        "About Pegboard",
        {"ko": ("Chromium 정보", "Pegboard 정보"), "en-GB": ("About Chromium", "About Pegboard")},
    ),
    "IDS_DEFAULT": (
        "Chromium is your default browser",
        "Pegboard is your default browser",
        {
            "ko": (
                "Chromium이 기본 브라우저이고 Chromium을 쓰며 Chromium은 좋고 Chromium으로 연다. "
                "Chromium에 권한을 주면 Chromium 이 창은 닫힌다",
                "Pegboard가 기본 브라우저이고 Pegboard를 쓰며 Pegboard는 좋고 Pegboard로 연다. "
                "Pegboard에 권한을 주면 Pegboard 이 창은 닫힌다",
            )
        },
    ),
    "IDS_OPEN": (
        'Open <ph name="URL">$1<ex>https://www.example.com/</ex></ph> in Chromium.',
        'Open <ph name="URL">$1<ex>https://www.example.com/</ex></ph> in Pegboard.',
        {"ko": ('Chromium에서 <ph name="URL" /> 열기', 'Pegboard에서 <ph name="URL" /> 열기')},
    ),
    "IDS_COPYRIGHT": (
        "Copyright 2026 The Chromium Authors. All rights reserved.",
        None,
        {"ko": ("Copyright 2026 The Chromium Authors. All rights reserved.", None)},
    ),
    "IDS_OS": (
        "About ChromiumOS and Chromium OS",
        None,
        {"ko": ("ChromiumOS 및 Chromium OS 정보", None)},
    ),
    "IDS_URL": (
        "See https://www.chromium.org/Chromium for Chromium help",
        "See https://www.chromium.org/Chromium for Pegboard help",
        {
            "ko": (
                "Chromium 도움말: https://www.chromium.org/Chromium",
                "Pegboard 도움말: https://www.chromium.org/Chromium",
            )
        },
    ),
    "IDS_NO_NAME": ("Nothing here", None, {"ko": ("여기 없음", None)}),
    "IDS_DROPPED_NAME": (
        "Relaunch Chromium now",
        "Relaunch Pegboard now",
        {"ko": ("지금 다시 시작", "지금 다시 시작")},
    ),
    "IDS_FIXED": ("Chromium", "Pegboard", {}),  # translateable="false"
}
CHROME_PART = {
    "IDS_PART_HELP": (
        "Get help with Chromium",
        "Get help with Pegboard",
        {"ko": ("Chromium 도움말 보기", "Pegboard 도움말 보기")},
    ),
}
COMPONENTS = {
    "IDS_LICENSE": (
        'Chromium is made possible by the <ph name="BEGIN_LINK_CHROMIUM">&lt;a href="$1"&gt;</ph>'
        'Chromium<ph name="END_LINK_CHROMIUM">&lt;/a&gt;</ph> open source project.',
        'Pegboard is made possible by the <ph name="BEGIN_LINK_CHROMIUM">&lt;a href="$1"&gt;</ph>'
        'Chromium<ph name="END_LINK_CHROMIUM">&lt;/a&gt;</ph> open source project.',
        {
            "ko": (
                'Chromium은 <ph name="BEGIN_LINK_CHROMIUM" />Chromium<ph name="END_LINK_CHROMIUM" />'
                " 오픈소스 프로젝트를 통해 제작된 브라우저입니다.",
                'Pegboard는 <ph name="BEGIN_LINK_CHROMIUM" />Chromium<ph name="END_LINK_CHROMIUM" />'
                " 오픈소스 프로젝트를 통해 제작된 브라우저입니다.",
            )
        },
    ),
}


def load_script():
    spec = importlib.util.spec_from_file_location("rebrand_strings", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def messages_xml(table, after):
    out = []
    for name, (before_text, after_text, _) in table.items():
        text = (after_text or before_text) if after else before_text
        extra = ' translateable="false"' if name == "IDS_FIXED" else ""
        out.append(f'      <message name="{name}" desc="d"{extra}>\n        {text}\n      </message>')
    return "\n".join(out)


class Tree:
    """A fake Chromium tree: tools/grit links to the real one."""

    def __init__(self, root):
        self.root = root
        os.makedirs(os.path.join(root, "tools"))
        os.symlink(os.path.join(SRC, "tools", "grit"), os.path.join(root, "tools", "grit"))
        self.grit = os.path.join(root, "tools", "grit")
        self.mod = load_script()

    def write(self, rel, text):
        path = os.path.join(self.root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
        return path

    def read(self, rel):
        with open(os.path.join(self.root, rel), encoding="utf-8") as f:
            return f.read()

    def grd(self, rel, xtb_dir, base, tables, part, after):
        """Writes a .grd (and a .grdp when part) and returns {name: id} by grit."""
        files = "\n".join(
            f'    <file path="{xtb_dir}/{base}_{lang}.xtb" lang="{lang}" />' for lang in ("ko", "en-GB")
        )
        main, part_table = tables
        messages = messages_xml(main, after)
        if part:
            messages += f'\n      <part file="{part}" />'
            self.write(os.path.join(os.path.dirname(rel), part), GRDP.format(messages=messages_xml(part_table, after)))
        path = self.write(rel, GRD.format(files=files, messages=messages))
        return dict(self.mod.message_ids(self.grit, path))

    def xtbs(self, rel, xtb_dir, base, table, ids, after):
        for lang in ("ko", "en-GB"):
            lines = []
            for name, (_, _, translations) in table.items():
                if lang in translations:
                    before_text, after_text = translations[lang]
                    text = (after_text or before_text) if after else before_text
                    lines.append(f'<translation id="{ids[name]}">{text}</translation>')
            self.write(
                os.path.join(os.path.dirname(rel), xtb_dir, f"{base}_{lang}.xtb"),
                XTB.format(lang=lang, translations="\n".join(lines)),
            )


CHROME_GRD = "chrome/app/chromium_strings.grd"
COMP_GRD = "components/components_chromium_strings.grd"


def build(root, after):
    """Writes the fake tree as it is before (or should be after) the script."""
    t = Tree(root)
    ids = t.grd(CHROME_GRD, "resources", "chromium_strings", (CHROME, CHROME_PART), "part.grdp", after)
    t.xtbs(CHROME_GRD, "resources", "chromium_strings", {**CHROME, **CHROME_PART}, ids, after)
    cids = t.grd(COMP_GRD, "strings", "components_chromium_strings", (COMPONENTS, {}), None, after)
    t.xtbs(COMP_GRD, "strings", "components_chromium_strings", COMPONENTS, cids, after)
    return t, {**ids, **cids}


def snapshot(root):
    out = {}
    for d, _, files in os.walk(root):
        if "tools" in os.path.relpath(d, root).split(os.sep):
            continue
        for f in files:
            p = os.path.join(d, f)
            with open(p, encoding="utf-8") as fh:
                out[os.path.relpath(p, root)] = fh.read()
    return out


def run(root):
    return subprocess.run(
        [sys.executable, SCRIPT, root], check=True, capture_output=True, text=True
    ).stdout


class RebrandStrings(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.before_tree, self.before_ids = build(os.path.join(self.tmp, "t"), after=False)
        _, self.expected_ids = build(os.path.join(self.tmp, "expected"), after=True)
        self.out = run(self.before_tree.root)

    def test_tree_matches_the_hand_written_result(self):
        got = snapshot(os.path.join(self.tmp, "t"))
        want = snapshot(os.path.join(self.tmp, "expected"))
        self.assertEqual(sorted(got), sorted(want))
        for rel in want:
            self.assertEqual(got[rel], want[rel], rel)

    def test_ids_change_only_where_the_english_changed(self):
        for name, (_, after_text, _) in {**CHROME, **CHROME_PART, **COMPONENTS}.items():
            if after_text is None:
                self.assertEqual(self.before_ids[name], self.expected_ids[name], name)
            else:
                self.assertNotEqual(self.before_ids[name], self.expected_ids[name], name)

    def test_xtb_ids_are_grits_ids_for_the_new_text(self):
        ko = self.before_tree.read("chrome/app/resources/chromium_strings_ko.xtb")
        for name in ("IDS_ABOUT", "IDS_DEFAULT", "IDS_PART_HELP", "IDS_COPYRIGHT"):
            self.assertIn(f'id="{self.expected_ids[name]}"', ko, name)
        self.assertNotIn(f'id="{self.before_ids["IDS_ABOUT"]}"', ko)

    def test_exclusions_stay(self):
        grd = self.before_tree.read(CHROME_GRD)
        self.assertIn("The Chromium Authors", grd)
        self.assertIn("About ChromiumOS and Chromium OS", grd)
        self.assertIn("https://www.chromium.org/Chromium", grd)
        comp = self.before_tree.read(COMP_GRD)
        self.assertIn("</ph>Chromium<ph", comp)

    def test_output_counts(self):
        self.assertIn(
            "chrome/app/chromium_strings.grd: 7 messages changed, 2 excluded", self.out
        )
        self.assertIn("ko 6", self.out)
        self.assertIn("components/components_chromium_strings.grd: 1 messages changed", self.out)

    def test_second_run_changes_nothing(self):
        first = snapshot(os.path.join(self.tmp, "t"))
        out = run(self.before_tree.root)
        self.assertEqual(snapshot(os.path.join(self.tmp, "t")), first)
        self.assertIn("0 messages changed", out)
        self.assertIn("0 message IDs rewritten", out)


def main():
    global SRC
    parser = argparse.ArgumentParser()
    parser.add_argument("--src", required=True, help="Chromium src dir (for tools/grit)")
    args, rest = parser.parse_known_args()
    SRC = os.path.abspath(args.src)
    if not os.path.isdir(os.path.join(SRC, "tools", "grit", "grit")):
        sys.exit(f"{SRC}/tools/grit not found")
    unittest.main(argv=[sys.argv[0]] + rest)


if __name__ == "__main__":
    main()
