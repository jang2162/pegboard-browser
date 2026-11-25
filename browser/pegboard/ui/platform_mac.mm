// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/platform.h"

#import <AppKit/AppKit.h>

namespace pegboard::platform {

bool AppIsActive() {
  return NSApp.active;
}

bool RequestAttentionIfInactive() {
  if (NSApp.active) {
    return false;
  }
  // Informational: the Dock icon bounces once (a critical request would
  // bounce until the app is activated). It does not activate the app.
  [NSApp requestUserAttention:NSInformationalRequest];
  return true;
}

KeyRelation KeyRelationOf(gfx::NativeWindow window) {
  NSWindow* ns_window = window.GetNativeNSWindow();
  NSWindow* key = NSApp.keyWindow;
  if (!ns_window || !key) {
    return KeyRelation::kNotKey;
  }
  if (key == ns_window) {
    return KeyRelation::kKey;
  }
  if (key.sheetParent == ns_window) {
    return KeyRelation::kSheetIsKey;
  }
  return KeyRelation::kNotKey;
}

bool HasKeyWindow() {
  return NSApp.keyWindow != nil;
}

}  // namespace pegboard::platform
