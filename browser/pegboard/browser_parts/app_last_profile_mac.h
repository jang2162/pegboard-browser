// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_BROWSER_PARTS_APP_LAST_PROFILE_MAC_H_
#define PEGBOARD_BROWSER_PARTS_APP_LAST_PROFILE_MAC_H_

#include <memory>

#include "base/functional/callback.h"

class Profile;

// The app's "last profile" on macOS (AppController's lastProfile): the
// profile of the last main browser window, which AppKit's paths without a
// window of their own use: links opened from other apps
// (application:openURLs:), the app menu's Settings and New Window, a Dock
// click with no window (RunInLastProfileSafely).
namespace pegboard::app_last_profile {

// AppController's lastProfileIfLoaded (null when there is none loaded).
Profile* Get();
// AppController's setLastProfile: (the menus follow it).
void Set(Profile* profile);

// Runs `changed` (in a task of its own, after AppController saw it) each time
// a window becomes the app's main window, until this is destroyed.
class MainWindowWatch {
 public:
  explicit MainWindowWatch(base::RepeatingClosure changed);
  MainWindowWatch(const MainWindowWatch&) = delete;
  MainWindowWatch& operator=(const MainWindowWatch&) = delete;
  ~MainWindowWatch();

 private:
  // The notification center's observer token.
  struct Token;
  std::unique_ptr<Token> token_;
};

}  // namespace pegboard::app_last_profile

#endif  // PEGBOARD_BROWSER_PARTS_APP_LAST_PROFILE_MAC_H_
