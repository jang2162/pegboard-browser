// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/test_hooks.h"

#include <memory>
#include <string_view>
#include <vector>

#include "base/environment.h"
#include "base/files/file_path.h"
#include "base/files/file_util.h"
#include "base/json/json_writer.h"
#include "base/strings/string_number_conversions.h"
#include "base/strings/string_split.h"
#include "base/strings/cstring_view.h"
#include "base/threading/thread_restrictions.h"
#include "pegboard/ui/buildflags.h"

namespace pegboard::test_hooks {

namespace {

constexpr char kHooks[] = "PEGBOARD_TEST_HOOKS";
constexpr char kLog[] = "PEGBOARD_TEST_DIALOG_LOG";
constexpr char kAnswer[] = "PEGBOARD_TEST_DIALOG_ANSWER";
constexpr char kAnswerAfter[] = "PEGBOARD_TEST_DIALOG_ANSWER_AFTER_MS";
constexpr char kInputAt[] = "PEGBOARD_TEST_DIALOG_INPUT_AT_MS";
constexpr base::TimeDelta kDefaultAnswerAfter = base::Milliseconds(1700);

// The value of the test variable `name`, only with test hooks on.
std::optional<std::string> Var(base::cstring_view name) {
  if (!Enabled()) {
    return std::nullopt;
  }
  return base::Environment::Create()->GetVar(name);
}

}  // namespace

bool Enabled() {
#if BUILDFLAG(PEGBOARD_TEST_HOOKS)
  return base::Environment::Create()->HasVar(kHooks);
#else
  return false;
#endif
}

void LogDialog(const base::DictValue& entry) {
  std::optional<std::string> path = Var(kLog);
  if (!path || path->empty()) {
    return;
  }
  std::optional<std::string> line = base::WriteJson(entry);
  if (!line) {
    return;
  }
  line->push_back('\n');
  // Written before the dialog's answer goes out, so a test that has the
  // answer finds the line. Test runs only.
  base::ScopedAllowBlockingForTesting allow_blocking;
  const base::FilePath file(*path);
  // AppendToFile does not make the file.
  if (!base::PathExists(file)) {
    base::WriteFile(file, *line);
    return;
  }
  base::AppendToFile(file, *line);
}

std::optional<std::string> AnswerFor(const std::string& session_name) {
  std::optional<std::string> spec = Var(kAnswer);
  if (!spec) {
    return std::nullopt;
  }
  std::optional<std::string> any;
  for (std::string_view item : base::SplitStringPiece(
           *spec, ",", base::TRIM_WHITESPACE, base::SPLIT_WANT_NONEMPTY)) {
    const size_t eq = item.rfind('=');
    if (eq == std::string_view::npos) {
      continue;
    }
    const std::string_view name = item.substr(0, eq);
    const std::string answer(item.substr(eq + 1));
    if (name == session_name) {
      return answer;
    }
    if (name == "*" && !any) {
      any = answer;
    }
  }
  return any;
}

base::TimeDelta AnswerAfter() {
  std::optional<std::string> ms = Var(kAnswerAfter);
  int value = 0;
  if (!ms || !base::StringToInt(*ms, &value) || value < 0) {
    return kDefaultAnswerAfter;
  }
  return base::Milliseconds(value);
}

std::vector<base::TimeDelta> InputAt() {
  std::vector<base::TimeDelta> at;
  std::optional<std::string> spec = Var(kInputAt);
  if (!spec) {
    return at;
  }
  for (std::string_view item : base::SplitStringPiece(
           *spec, ",", base::TRIM_WHITESPACE, base::SPLIT_WANT_NONEMPTY)) {
    int ms = 0;
    if (base::StringToInt(item, &ms) && ms >= 0) {
      at.push_back(base::Milliseconds(ms));
    }
  }
  return at;
}

}  // namespace pegboard::test_hooks
