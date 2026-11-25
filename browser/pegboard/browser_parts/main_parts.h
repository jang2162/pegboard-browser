// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_BROWSER_PARTS_MAIN_PARTS_H_
#define PEGBOARD_BROWSER_PARTS_MAIN_PARTS_H_

#include "chrome/browser/chrome_browser_main_extra_parts.h"
#include "pegboard/bootstrap/daemon_launcher.h"
#include "pegboard/browser_parts/agent_profiles.h"

namespace pegboard {

// The one ChromeBrowserMainExtraParts pegboard-bootstrap.patch adds to
// Chrome's main parts (macOS): Pegboard's browser-wide parts, each given the
// stages of the browser's start and end it needs.
// - ProfileDefaults: sign-in and the password manager off by default in every
//   profile (registered in PreProfileInit, before the first profile).
// - AgentProfiles: agent profiles hidden and marked (from PreProfileInit, so
//   that the startup profile picker decision already leaves them out).
// - DaemonLauncher: pegboardd with the DevTools pipe (PostBrowserStart to
//   PostMainMessageLoopRun).
class MainParts : public ChromeBrowserMainExtraParts {
 public:
  MainParts();
  MainParts(const MainParts&) = delete;
  MainParts& operator=(const MainParts&) = delete;
  ~MainParts() override;

  // ChromeBrowserMainExtraParts:
  void PreProfileInit() override;
  void PostProfileInit(Profile* profile, bool is_initial_profile) override;
  void PostBrowserStart() override;
  void PostMainMessageLoopRun() override;

 private:
  AgentProfiles agent_profiles_;
  DaemonLauncher launcher_;
};

}  // namespace pegboard

#endif  // PEGBOARD_BROWSER_PARTS_MAIN_PARTS_H_
