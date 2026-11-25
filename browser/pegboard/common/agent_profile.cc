// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/common/agent_profile.h"

#include "base/strings/string_util.h"

namespace pegboard {

bool IsAgentDirectoryName(std::string_view dir) {
  return dir == kAgentDirectory || base::StartsWith(dir, kAgentDirectoryPrefix);
}

}  // namespace pegboard
