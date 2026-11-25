// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_WINDOW_FOCUS_H_
#define PEGBOARD_UI_WINDOW_FOCUS_H_

#include <string>

class BrowserWindowInterface;

namespace pegboard {

// The profile directory of `browser`'s profile ("" without one).
std::string ProfileDirectoryOf(BrowserWindowInterface* browser);

// For the test hooks' logs: the profile directory of the app's key window
// ("sheet:<dir>" when a sheet attached to that window is key), "none" when
// there is no key window, "other" when it is no browser window's (one of
// Pegboard's own dialogs).
std::string KeyWindowProfile();

}  // namespace pegboard

#endif  // PEGBOARD_UI_WINDOW_FOCUS_H_
