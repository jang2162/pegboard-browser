// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_TEST_HOOKS_H_
#define PEGBOARD_UI_TEST_HOOKS_H_

#include <optional>
#include <string>

#include "base/time/time.h"
#include "base/values.h"

// Test hooks of the browser's own windows, read from the browser's own
// environment. Every one needs PEGBOARD_TEST_HOOKS there as well: a product
// launch (Dock, open) has no such variable, and without it each function here
// does nothing (Enabled() false, the rest empty). Nothing here answers for a
// person on its own: an answer hook presses a dialog's button through the
// same function a click runs, so the dialog's own rules (its 1 s lock) hold.
namespace pegboard::test_hooks {

// PEGBOARD_TEST_HOOKS is set in the browser's environment.
bool Enabled();

// Appends `entry` as one JSON line to the file PEGBOARD_TEST_DIALOG_LOG names.
void LogDialog(const base::DictValue& entry);

// The answer PEGBOARD_TEST_DIALOG_ANSWER gives a dialog of the session named
// `session_name`: a comma-separated list of "<session name>=<answer>", where
// "*" stands for any other name and an answer is "approve", "deny",
// "allowForSession" or "none" (press nothing). The first entry for the name
// counts, else the first "*". Nothing when no entry fits.
std::optional<std::string> AnswerFor(const std::string& session_name);

// When the answer hook presses, after the dialog is shown:
// PEGBOARD_TEST_DIALOG_ANSWER_AFTER_MS, by default 1100 ms (just after the
// dialog's 1 s lock).
base::TimeDelta AnswerAfter();

}  // namespace pegboard::test_hooks

#endif  // PEGBOARD_UI_TEST_HOOKS_H_
