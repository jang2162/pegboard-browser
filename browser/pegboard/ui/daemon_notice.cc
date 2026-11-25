// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/daemon_notice.h"

#include <algorithm>
#include <optional>
#include <utility>

#include "base/functional/bind.h"
#include "base/task/single_thread_task_runner.h"
#include "base/time/time.h"
#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "chrome/browser/ui/browser_window/public/desktop_browser_window_capabilities.h"
#include "chrome/browser/ui/browser_window/public/global_browser_collection.h"
#include "pegboard/common/agent_profile.h"
#include "pegboard/ui/platform.h"
#include "pegboard/ui/test_hooks.h"
#include "pegboard/ui/window_focus.h"
#include "ui/base/base_window.h"
#include "ui/base/mojom/dialog_button.mojom.h"
#include "ui/base/mojom/ui_base_types.mojom-shared.h"
#include "ui/views/controls/label.h"
#include "ui/views/layout/layout_provider.h"
#include "ui/views/style/typography.h"
#include "ui/views/window/dialog_delegate.h"

namespace pegboard {

namespace {

using ui::mojom::DialogButton;

constexpr char16_t kTitle[] = u"Pegboard 에이전트 데몬이 멈췄습니다";
constexpr char16_t kBody[] =
    u"데몬이 1분 안에 네 번 끝나서 다시 띄우지 않습니다. 에이전트 기능을 다시 "
    u"쓰려면 다시 시작하세요.";
constexpr char16_t kRestartLabel[] = u"다시 시작";
constexpr char16_t kCloseLabel[] = u"닫기";

// When the test hook's log samples the key window a second time.
constexpr base::TimeDelta kFocusSample = base::Milliseconds(200);
// A window that is not on screen yet (just made) is tried again this often,
// this many times; then the notice goes over it anyway.
constexpr base::TimeDelta kRetry = base::Milliseconds(250);
constexpr int kMaxRetries = 20;
// How far below the top of the window the notice sits (under its toolbar).
constexpr int kTopOffset = 90;

// A person's normal window that is not closing: not an agent profile's (by the
// agent profile rule, of the original profile for an incognito window).
bool IsPersonsWindow(BrowserWindowInterface* browser) {
  if (!browser || browser->GetType() != BrowserWindowInterface::TYPE_NORMAL ||
      browser->capabilities()->IsAttemptingToCloseBrowser()) {
    return false;
  }
  Profile* profile = browser->GetProfile();
  return profile && !IsAgentDirectoryName(
                        profile->GetOriginalProfile()->GetBaseName().AsUTF8Unsafe());
}

bool IsOnScreen(BrowserWindowInterface* browser) {
  ui::BaseWindow* window = browser->GetWindow();
  return window && window->IsVisible() && !window->IsMinimized();
}

}  // namespace

DaemonNotice::DaemonNotice(base::OnceClosure restart)
    : restart_(std::move(restart)) {
  browsers_observation_.Observe(GlobalBrowserCollection::GetInstance());
  TryShow();
}

DaemonNotice::~DaemonNotice() {
  Close("destroyed");
}

void DaemonNotice::OnBrowserCreated(BrowserWindowInterface* browser) {
  if (state_ != State::kWaiting || !IsPersonsWindow(browser)) {
    return;
  }
  retries_ = 0;
  // Not from inside the window's creation.
  retry_timer_.Start(FROM_HERE, base::TimeDelta(),
                     base::BindOnce(&DaemonNotice::TryShow,
                                    base::Unretained(this)));
}

void DaemonNotice::TryShow() {
  if (state_ != State::kWaiting) {
    return;
  }
  BrowserWindowInterface* on_screen = nullptr;
  BrowserWindowInterface* any = nullptr;
  ForEachCurrentBrowserWindowInterfaceOrderedByActivation(
      [&](BrowserWindowInterface* browser) {
        if (!IsPersonsWindow(browser)) {
          return true;
        }
        if (!any) {
          any = browser;
        }
        if (IsOnScreen(browser)) {
          on_screen = browser;
          return false;
        }
        return true;
      });
  BrowserWindowInterface* over = on_screen;
  if (!over && any && retries_ >= kMaxRetries) {
    over = any;
  }
  if (over && ShowOver(over)) {
    return;
  }
  if (any && retries_ < kMaxRetries) {
    ++retries_;
    retry_timer_.Start(FROM_HERE, kRetry,
                       base::BindOnce(&DaemonNotice::TryShow,
                                      base::Unretained(this)));
  }
  // Otherwise the next person's window that opens (OnBrowserCreated).
}

bool DaemonNotice::ShowOver(BrowserWindowInterface* browser) {
  ui::BaseWindow* window = browser->GetWindow();
  views::Widget* parent =
      window ? views::Widget::GetWidgetForNativeWindow(window->GetNativeWindow())
             : nullptr;
  if (!parent) {
    return false;
  }
  views::LayoutProvider* layout = views::LayoutProvider::Get();
  const int width =
      layout->GetDistanceMetric(views::DISTANCE_MODAL_DIALOG_PREFERRED_WIDTH);
  const gfx::Insets margins = layout->GetDialogInsetsForContentType(
      views::DialogContentType::kText, views::DialogContentType::kText);

  delegate_ = std::make_unique<views::DialogDelegate>();
  delegate_->SetOwnershipOfNewWidget(
      views::Widget::InitParams::CLIENT_OWNS_WIDGET);
  delegate_->SetModalType(ui::mojom::ModalType::kNone);
  delegate_->SetTitle(kTitle);
  delegate_->SetShowCloseButton(false);
  delegate_->set_fixed_width(width);
  delegate_->set_margins(margins);
  auto body = std::make_unique<views::Label>(
      kBody, views::style::CONTEXT_DIALOG_BODY_TEXT,
      views::style::STYLE_PRIMARY);
  body->SetMultiLine(true);
  body->SetHorizontalAlignment(gfx::ALIGN_LEFT);
  body->SetMaximumWidth(width - margins.width());
  delegate_->SetContentsView(std::move(body));
  delegate_->SetButtons(static_cast<int>(DialogButton::kOk) |
                        static_cast<int>(DialogButton::kCancel));
  delegate_->SetButtonLabel(DialogButton::kOk, kRestartLabel);
  delegate_->SetButtonLabel(DialogButton::kCancel, kCloseLabel);
  // Return closes it; restarting takes the button.
  delegate_->SetDefaultButton(static_cast<int>(DialogButton::kCancel));
  delegate_->SetAcceptCallbackWithClose(base::BindRepeating(
      [](DaemonNotice* n) {
        n->Press(true, "button");
        return false;
      },
      base::Unretained(this)));
  // The close button, Return and Esc.
  delegate_->SetCancelCallbackWithClose(base::BindRepeating(
      [](DaemonNotice* n) {
        n->Press(false, "button");
        return false;
      },
      base::Unretained(this)));

  const bool hooks = test_hooks::Enabled();
  if (hooks) {
    focus_.Set("keyWindowProfileBefore", KeyWindowProfile());
    focus_.Set("appActiveBefore", platform::AppIsActive());
  }
  widget_.reset(views::DialogDelegate::CreateDialogWidget(
      delegate_.get(), gfx::NativeWindow(), parent->GetNativeView()));
  if (!widget_) {
    delegate_.reset();
    return false;
  }
  widget_observation_.Observe(widget_.get());
  const gfx::Rect over = parent->GetWindowBoundsInScreen();
  const gfx::Size size = widget_->GetRootView()->GetPreferredSize({});
  widget_->SetBounds(gfx::Rect(
      over.x() + (over.width() - size.width()) / 2,
      over.y() + std::max(0, std::min(kTopOffset,
                                      over.height() - size.height())),
      size.width(), size.height()));
  // Never Show(): that makes the notice the key window and activates the app.
  widget_->ShowInactive();
  state_ = State::kShown;
  profile_ = ProfileDirectoryOf(browser);
  browsers_observation_.Reset();
  const bool attention = platform::RequestAttentionIfInactive();

  if (hooks) {
    focus_.Set("keyWindowProfileAt0", KeyWindowProfile());
    focus_.Set("appActiveAt0", platform::AppIsActive());
    focus_.Set("attention", attention);
    sample_timer_.Start(FROM_HERE, kFocusSample,
                        base::BindOnce(&DaemonNotice::SampleFocus,
                                       base::Unretained(this)));
    // The answer hook: "*=restart" or "*=close" (any other name does not
    // fit: the notice is of no session).
    std::optional<std::string> answer = test_hooks::AnswerFor(std::string());
    if (answer == "restart" || answer == "close") {
      hook_timer_.Start(FROM_HERE, test_hooks::AnswerAfter(),
                        base::BindOnce(&DaemonNotice::Press,
                                       base::Unretained(this),
                                       *answer == "restart", "hook"));
    }
  }
  return true;
}

void DaemonNotice::SampleFocus() {
  if (state_ != State::kShown) {
    return;
  }
  focus_.Set("keyWindowProfileAt200ms", KeyWindowProfile());
  focus_.Set("appActiveAt200ms", platform::AppIsActive());
  base::DictValue entry;
  entry.Set("kind", "daemon_stopped");
  entry.Set("profile", profile_);
  entry.Merge(focus_.Clone());
  test_hooks::LogDialog(entry);
}

void DaemonNotice::Press(bool restart, std::string_view by) {
  if (state_ != State::kShown) {
    return;
  }
  state_ = State::kClosed;
  sample_timer_.Stop();
  hook_timer_.Stop();
  LogClosed(restart ? "restart" : "close", by, "answered");
  // Not from inside the widget's own event handling: `restart` may delete
  // this.
  base::SingleThreadTaskRunner::GetCurrentDefault()->PostTask(
      FROM_HERE, base::BindOnce(&DaemonNotice::Finish,
                                weak_factory_.GetWeakPtr(), restart));
}

void DaemonNotice::Finish(bool restart) {
  widget_observation_.Reset();
  widget_.reset();
  delegate_.reset();
  if (!restart || !restart_) {
    return;
  }
  base::OnceClosure run = std::move(restart_);
  // May delete this.
  std::move(run).Run();
}

void DaemonNotice::Close(std::string_view why) {
  weak_factory_.InvalidateWeakPtrs();
  if (state_ == State::kShown) {
    LogClosed("", "", why);
  }
  state_ = State::kClosed;
  retry_timer_.Stop();
  sample_timer_.Stop();
  hook_timer_.Stop();
  restart_.Reset();
  browsers_observation_.Reset();
  widget_observation_.Reset();
  widget_.reset();
  delegate_.reset();
}

void DaemonNotice::OnWidgetDestroying(views::Widget* widget) {
  // Its window closed: the notice went with it (no restart).
  widget_observation_.Reset();
  if (state_ == State::kShown) {
    LogClosed("", "", "window_closed");
    state_ = State::kClosed;
    sample_timer_.Stop();
    hook_timer_.Stop();
  }
}

void DaemonNotice::LogClosed(std::string_view pressed,
                             std::string_view by,
                             std::string_view why) {
  if (!test_hooks::Enabled()) {
    return;
  }
  base::DictValue entry;
  entry.Set("kind", "daemon_notice_closed");
  entry.Set("profile", profile_);
  if (pressed.empty()) {
    entry.Set("pressed", base::Value());
  } else {
    entry.Set("pressed", pressed);
  }
  if (!by.empty()) {
    entry.Set("by", by);
  }
  entry.Set("closed", why);
  test_hooks::LogDialog(entry);
}

}  // namespace pegboard
