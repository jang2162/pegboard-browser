// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_COMMON_AGENT_PROFILE_H_
#define PEGBOARD_COMMON_AGENT_PROFILE_H_

#include <string_view>

namespace pegboard {

inline constexpr std::string_view kAgentDirectory = "PegboardAgent";
inline constexpr std::string_view kAgentDirectoryPrefix = "PegboardAgent-";

// The agent profile rule (F10): a profile directory named exactly
// "PegboardAgent" or starting with "PegboardAgent-". Every other profile is a
// person's.
bool IsAgentDirectoryName(std::string_view dir);

}  // namespace pegboard

#endif  // PEGBOARD_COMMON_AGENT_PROFILE_H_
