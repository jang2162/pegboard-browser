// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_UI_CONFIRM_DIALOG_H_
#define PEGBOARD_UI_CONFIRM_DIALOG_H_

#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "base/functional/callback.h"
#include "base/memory/raw_ptr.h"
#include "base/memory/weak_ptr.h"
#include "base/scoped_observation.h"
#include "base/time/time.h"
#include "base/timer/timer.h"
#include "base/values.h"
#include "ui/views/widget/widget.h"
#include "ui/views/widget/widget_observer.h"

class BrowserWindowInterface;

namespace views {
class DialogDelegate;
class MdTextButton;
}  // namespace views

namespace pegboard {

enum class ConfirmAnswer { kApprove, kDeny, kAllowForSession };

// The wire names of Pegboard.confirmationAnswered: "approve", "deny",
// "allowForSession".
std::string_view AnswerName(ConfirmAnswer answer);
std::optional<ConfirmAnswer> AnswerOf(std::string_view name);

// Pegboard.requestConfirmation's parameters (pegboard_protocol.pdl), as the
// domain checked them. `details` in the daemon's order.
struct ConfirmRequest {
  ConfirmRequest();
  ConfirmRequest(ConfirmRequest&&);
  ConfirmRequest& operator=(ConfirmRequest&&);
  ~ConfirmRequest();

  std::string confirm_id;
  std::string session_id;
  std::string session_name;
  std::string profile_directory;
  // "upload" or "state".
  std::string action_class;
  // (label, value)
  std::vector<std::pair<std::string, std::string>> details;
  bool offer_allow_for_session = false;
};

// A person's confirmation: a dialog of browser UI (not web content), a child
// window over `browser`'s window. It is shown without activating anything: no
// app activation, no window brought to the front, no key window change. (Not
// window-modal: on macOS that is a sheet, which AppKit makes the key window.) When the app is not the active
// one the Dock icon bounces once.
//
// Its buttons are 허용 (approve), 거부 (deny, the default button: Return and
// Esc are 거부) and, when offered, 이 세션에서 계속 허용 (allowForSession).
// For 1 s after it is shown every press is ignored, so that keys or a click a
// person meant for something else do not answer it.
//
// `done` runs once, in a task of its own, with the answer; the dialog closing
// in any other way than Close() (its window or tab closing, Deny()) is kDeny.
// It never runs after Close() or the dialog's destruction.
class ConfirmDialog : public views::WidgetObserver {
 public:
  using Done = base::OnceCallback<void(ConfirmAnswer)>;

  // The dialog on `browser`'s window; nullptr when it cannot be shown there.
  static std::unique_ptr<ConfirmDialog> Show(BrowserWindowInterface* browser,
                                             ConfirmRequest request,
                                             Done done);

  ConfirmDialog(const ConfirmDialog&) = delete;
  ConfirmDialog& operator=(const ConfirmDialog&) = delete;
  // As Close("destroyed").
  ~ConfirmDialog() override;

  // Closes it without an answer (`why` goes to the test hook's log).
  void Close(std::string_view why);
  // Closes it with kDeny, as closing it would (also during the lock).
  void Deny(std::string_view why);

  // views::WidgetObserver:
  void OnWidgetClosing(views::Widget* widget) override;
  void OnWidgetDestroying(views::Widget* widget) override;

 private:
  enum class State { kOpen, kAnswered, kClosed };

  ConfirmDialog(ConfirmRequest request, Done done);

  bool Open(BrowserWindowInterface* browser);
  // A button (or the answer hook, `by` "hook"): ignored while locked.
  void Press(ConfirmAnswer answer, std::string_view by);
  void Finish(ConfirmAnswer answer, std::string_view by, std::string_view why);
  void RunDone(ConfirmAnswer answer);
  void Unlock();
  void SampleFocus();
  void DestroyWidget();
  void Log(std::optional<ConfirmAnswer> answer,
           std::string_view by,
           std::string_view why);

  const ConfirmRequest request_;
  Done done_;
  State state_ = State::kOpen;
  std::string profile_;
  base::TimeTicks shown_at_;
  // What the test hook's log records (only with test hooks on).
  base::ListValue lines_;
  base::ListValue buttons_;
  base::DictValue focus_;
  bool ignored_locked_ = false;

  // The widget goes before its delegate.
  std::unique_ptr<views::DialogDelegate> delegate_;
  std::unique_ptr<views::Widget> widget_;
  raw_ptr<views::MdTextButton> allow_for_session_button_ = nullptr;
  base::ScopedObservation<views::Widget, views::WidgetObserver>
      widget_observation_{this};
  base::OneShotTimer unlock_timer_;
  base::OneShotTimer sample_timer_;
  base::OneShotTimer hook_timer_;
  base::WeakPtrFactory<ConfirmDialog> weak_factory_{this};
};

}  // namespace pegboard

#endif  // PEGBOARD_UI_CONFIRM_DIALOG_H_
