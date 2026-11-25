// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_DAEMON_NOTICE_H_
#define PEGBOARD_UI_DAEMON_NOTICE_H_

#include <memory>
#include <string>
#include <string_view>

#include "base/functional/callback.h"
#include "base/memory/weak_ptr.h"
#include "base/scoped_observation.h"
#include "base/timer/timer.h"
#include "base/values.h"
#include "chrome/browser/ui/browser_window/public/browser_collection.h"
#include "chrome/browser/ui/browser_window/public/browser_collection_observer.h"
#include "ui/views/widget/widget.h"
#include "ui/views/widget/widget_observer.h"

class BrowserWindowInterface;

namespace views {
class DialogDelegate;
}  // namespace views

namespace pegboard {

// The notice that the daemon will not be started again (design B16): a small
// window of the browser's own over the person's most recently active normal
// window, "Pegboard 에이전트 데몬이 멈췄습니다" with 다시 시작 (restart) and
// 닫기 (close). Like the confirmation dialog, it is a child window shown
// inactive (not window-modal: on macOS that is a sheet, which takes the key
// window), so it takes no keyboard focus and does not activate the app; the
// Dock icon bounces once when the app is not active. With no person's window
// open, it shows on the next one that opens.
//
// `restart` runs once, when 다시 시작 is pressed (in a task of its own, after
// the notice closed; it may delete the notice). Closing it any other way, or
// deleting it, runs nothing.
//
// Test hooks (test_hooks.h): a {"kind":"daemon_stopped", "profile",
// "keyWindowProfile{Before,At0,At200ms}", "appActive{Before,At0,At200ms}",
// "attention"} line 200 ms after it shows, a {"kind":"daemon_notice_closed",
// "pressed", "by", "closed"} line when it closes, and the answer hook
// (PEGBOARD_TEST_DIALOG_ANSWER "*=restart" or "*=close") presses a button.
class DaemonNotice : public BrowserCollectionObserver,
                     public views::WidgetObserver {
 public:
  explicit DaemonNotice(base::OnceClosure restart);
  DaemonNotice(const DaemonNotice&) = delete;
  DaemonNotice& operator=(const DaemonNotice&) = delete;
  // Closes the notice (without running `restart`).
  ~DaemonNotice() override;

  // BrowserCollectionObserver:
  void OnBrowserCreated(BrowserWindowInterface* browser) override;

  // views::WidgetObserver:
  void OnWidgetDestroying(views::Widget* widget) override;

 private:
  enum class State { kWaiting, kShown, kClosed };

  // Shows the notice over the person's most recently active normal window, if
  // there is one that is on screen; else waits for one.
  void TryShow();
  bool ShowOver(BrowserWindowInterface* browser);
  void Press(bool restart, std::string_view by);
  void Finish(bool restart);
  void Close(std::string_view why);
  void SampleFocus();
  void LogClosed(std::string_view pressed, std::string_view by,
                 std::string_view why);

  base::OnceClosure restart_;
  State state_ = State::kWaiting;
  std::string profile_;
  std::unique_ptr<views::DialogDelegate> delegate_;
  std::unique_ptr<views::Widget> widget_;
  base::ScopedObservation<views::Widget, views::WidgetObserver>
      widget_observation_{this};
  base::ScopedObservation<BrowserCollection, BrowserCollectionObserver>
      browsers_observation_{this};
  // A new window is not on screen at once: tried again a few times.
  base::OneShotTimer retry_timer_;
  int retries_ = 0;
  base::OneShotTimer sample_timer_;
  base::OneShotTimer hook_timer_;
  base::DictValue focus_;
  base::WeakPtrFactory<DaemonNotice> weak_factory_{this};
};

}  // namespace pegboard

#endif  // PEGBOARD_UI_DAEMON_NOTICE_H_
