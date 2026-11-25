// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/browser_parts/app_last_profile_mac.h"

#import <AppKit/AppKit.h>

#include <utility>

#include "base/location.h"
#include "base/task/single_thread_task_runner.h"
#include "chrome/browser/app_controller_mac.h"

namespace pegboard::app_last_profile {

Profile* Get() {
  return AppController.sharedController.lastProfileIfLoaded;
}

void Set(Profile* profile) {
  [AppController.sharedController setLastProfile:profile];
}

struct MainWindowWatch::Token {
  id __strong observer;
};

MainWindowWatch::MainWindowWatch(base::RepeatingClosure changed)
    : token_(std::make_unique<Token>()) {
  // AppController's own observer (windowDidBecomeMain:) may run before or after
  // this one: the callback is posted, so it runs after both.
  token_->observer = [NSNotificationCenter.defaultCenter
      addObserverForName:NSWindowDidBecomeMainNotification
                  object:nil
                   queue:nil
              usingBlock:^(NSNotification*) {
                base::SingleThreadTaskRunner::GetCurrentDefault()->PostTask(
                    FROM_HERE, changed);
              }];
}

MainWindowWatch::~MainWindowWatch() {
  [NSNotificationCenter.defaultCenter removeObserver:token_->observer];
}

}  // namespace pegboard::app_last_profile
