// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/browser_parts/main_parts.h"

#include "pegboard/browser_parts/profile_defaults.h"

namespace pegboard {

MainParts::MainParts() = default;

MainParts::~MainParts() = default;

void MainParts::PreProfileInit() {
  ProfileDefaults::Register();
  agent_profiles_.Start();
}

void MainParts::PostProfileInit(Profile* profile, bool is_initial_profile) {
  agent_profiles_.OnProfileInit(profile);
}

void MainParts::PostBrowserStart() {
  launcher_.PostBrowserStart();
}

void MainParts::PostMainMessageLoopRun() {
  agent_profiles_.Stop();
  launcher_.PostMainMessageLoopRun();
}

}  // namespace pegboard
