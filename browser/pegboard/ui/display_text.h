// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_DISPLAY_TEXT_H_
#define PEGBOARD_UI_DISPLAY_TEXT_H_

#include <cstddef>
#include <string>
#include <string_view>

namespace pegboard {

// Text an agent may choose (a session's name, a file's, a cookie's), as plain
// label text. Control, format, bidirectional and invisible characters become
// "\u{…}" (lowercase hex), the daemon's display_text rule
// (crates/pegboard-core/src/confirm/details.rs); at most `max_chars`
// characters are kept (an escape whole or not at all), then U+2026.
//
// Unlike the daemon's rule a backslash stays as it is: the daemon already
// doubled each one, so what it sends shows exactly as it sent it. This pass is
// a second wall against characters that change how text looks, not a second
// layer of escapes. Malformed UTF-8 shows as U+FFFD.
std::u16string DisplayText(std::string_view utf8, size_t max_chars);

// The number of characters (code points) of `utf8`.
size_t CharCount(std::string_view utf8);

}  // namespace pegboard

#endif  // PEGBOARD_UI_DISPLAY_TEXT_H_
