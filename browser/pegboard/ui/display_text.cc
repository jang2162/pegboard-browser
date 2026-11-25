// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/display_text.h"

#include "base/strings/stringprintf.h"
#include "base/strings/utf_string_conversion_utils.h"
#include "base/strings/utf_string_conversions.h"
#include "base/third_party/icu/icu_utf.h"

namespace pegboard {

namespace {

constexpr base_icu::UChar32 kReplacement = 0xFFFD;
constexpr char16_t kEllipsis = 0x2026;

// Unicode category Cc (Rust's char::is_control).
bool IsControl(base_icu::UChar32 c) {
  return c <= 0x1F || (c >= 0x7F && c <= 0x9F);
}

// Format characters (category Cf) and the line and paragraph separators: the
// daemon's is_format, the same ranges.
bool IsFormat(base_icu::UChar32 c) {
  struct Range {
    base_icu::UChar32 first, last;
  };
  static constexpr Range kRanges[] = {
      {0x00AD, 0x00AD},   {0x0600, 0x0605},   {0x061C, 0x061C},
      {0x06DD, 0x06DD},   {0x070F, 0x070F},   {0x0890, 0x0891},
      {0x08E2, 0x08E2},   {0x180E, 0x180E},   {0x200B, 0x200F},
      {0x2028, 0x202E},   {0x2060, 0x2064},   {0x2066, 0x206F},
      {0xFEFF, 0xFEFF},   {0xFFF9, 0xFFFB},   {0x110BD, 0x110BD},
      {0x110CD, 0x110CD}, {0x13430, 0x1343F}, {0x1BCA0, 0x1BCA3},
      {0x1D173, 0x1D17A}, {0xE0001, 0xE0001}, {0xE0020, 0xE007F},
  };
  for (const Range& r : kRanges) {
    if (c >= r.first && c <= r.last) {
      return true;
    }
  }
  return false;
}

// Characters that show nothing although they are not format characters (the
// daemon's is_invisible): Hangul fillers, the combining grapheme joiner and
// the variation selectors.
bool IsInvisible(base_icu::UChar32 c) {
  return c == 0x034F || c == 0x115F || c == 0x1160 || c == 0x3164 ||
         c == 0xFFA0 || (c >= 0xFE00 && c <= 0xFE0F) ||
         (c >= 0xE0100 && c <= 0xE01EF);
}

// One character as shown.
std::u16string Shown(base_icu::UChar32 c) {
  if (IsControl(c) || IsFormat(c) || IsInvisible(c)) {
    return base::ASCIIToUTF16(base::StringPrintf("\\u{%x}", c));
  }
  std::u16string out;
  base::WriteUnicodeCharacter(c, &out);
  return out;
}

}  // namespace

std::u16string DisplayText(std::string_view utf8, size_t max_chars) {
  std::u16string out;
  size_t kept = 0;
  for (size_t i = 0; i < utf8.size(); ++i) {
    base_icu::UChar32 c;
    if (!base::ReadUnicodeCharacter(utf8, &i, &c)) {
      c = kReplacement;
    }
    const std::u16string piece = Shown(c);
    // Each escape and each character here is one UTF-16 unit per character,
    // apart from a character outside the BMP (two units, one character).
    const size_t n = c > 0xFFFF && piece.size() == 2 ? 1 : piece.size();
    if (kept + n > max_chars) {
      out.push_back(kEllipsis);
      return out;
    }
    out += piece;
    kept += n;
  }
  return out;
}

size_t CharCount(std::string_view utf8) {
  size_t n = 0;
  for (size_t i = 0; i < utf8.size(); ++i) {
    base_icu::UChar32 c;
    base::ReadUnicodeCharacter(utf8, &i, &c);
    ++n;
  }
  return n;
}

}  // namespace pegboard
