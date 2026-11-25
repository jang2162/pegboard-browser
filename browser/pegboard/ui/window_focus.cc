// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/window_focus.h"

#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "pegboard/ui/platform.h"
#include "ui/base/base_window.h"

namespace pegboard {

std::string ProfileDirectoryOf(BrowserWindowInterface* browser) {
  Profile* profile = browser ? browser->GetProfile() : nullptr;
  return profile ? profile->GetBaseName().AsUTF8Unsafe() : std::string();
}

std::string KeyWindowProfile() {
  if (!platform::HasKeyWindow()) {
    return "none";
  }
  for (BrowserWindowInterface* browser : GetAllBrowserWindowInterfaces()) {
    ui::BaseWindow* window = browser->GetWindow();
    if (!window) {
      continue;
    }
    switch (platform::KeyRelationOf(window->GetNativeWindow())) {
      case platform::KeyRelation::kKey:
        return ProfileDirectoryOf(browser);
      case platform::KeyRelation::kSheetIsKey:
        return "sheet:" + ProfileDirectoryOf(browser);
      case platform::KeyRelation::kNotKey:
        break;
    }
  }
  return "other";
}

}  // namespace pegboard
