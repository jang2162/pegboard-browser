// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_PLATFORM_H_
#define PEGBOARD_UI_PLATFORM_H_

#include "ui/gfx/native_ui_types.h"

// What the confirmation dialog asks of the platform (macOS: AppKit).
namespace pegboard::platform {

// Whether this app is the active (frontmost) one.
bool AppIsActive();

// One Dock bounce (NSInformationalRequest), only when the app is not the
// active one. Activates nothing. Returns whether it asked.
bool RequestAttentionIfInactive();

// How `window` stands to the app's key window.
enum class KeyRelation {
  kNotKey,
  // `window` is the key window.
  kKey,
  // A sheet attached to `window` is the key window.
  kSheetIsKey,
};
KeyRelation KeyRelationOf(gfx::NativeWindow window);

// Whether the app has a key window at all.
bool HasKeyWindow();

}  // namespace pegboard::platform

#endif  // PEGBOARD_UI_PLATFORM_H_
