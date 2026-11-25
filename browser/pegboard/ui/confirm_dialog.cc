// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/ui/confirm_dialog.h"

#include <algorithm>

#include "base/functional/bind.h"
#include "base/strings/strcat.h"
#include "base/strings/string_number_conversions.h"
#include "base/strings/utf_string_conversions.h"
#include "base/task/single_thread_task_runner.h"
#include "chrome/browser/picture_in_picture/scoped_picture_in_picture_occlusion_observation.h"
#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "pegboard/ui/display_text.h"
#include "pegboard/ui/platform.h"
#include "pegboard/ui/test_hooks.h"
#include "ui/base/base_window.h"
#include "ui/base/mojom/dialog_button.mojom.h"
#include "ui/base/mojom/ui_base_types.mojom-shared.h"
#include "ui/gfx/text_constants.h"
#include "ui/events/event.h"
#include "ui/views/border.h"
#include "ui/views/controls/button/md_text_button.h"
#include "ui/views/controls/label.h"
#include "ui/views/controls/scroll_view.h"
#include "ui/views/layout/box_layout.h"
#include "ui/views/layout/box_layout_view.h"
#include "ui/views/layout/layout_provider.h"
#include "ui/views/style/typography.h"
#include "ui/views/view_class_properties.h"
#include "ui/views/window/dialog_client_view.h"
#include "ui/views/window/dialog_delegate.h"

namespace pegboard {

namespace {

using ui::mojom::DialogButton;

// Presses are ignored at least this long after the dialog is shown...
constexpr base::TimeDelta kLock = base::Seconds(1);
// ...and until this long has passed without a key or a click on the dialog
// (a person clicking or typing on and on does not answer it) ...
constexpr base::TimeDelta kQuiet = base::Milliseconds(500);
// ...and then this long more once the buttons are enabled, and as long after
// the dialog stops being covered by a picture-in-picture window.
constexpr base::TimeDelta kAfterUnlock = base::Milliseconds(500);
// When the test hook's log samples the key window a second time.
constexpr base::TimeDelta kFocusSample = base::Milliseconds(200);

// The most of a session's name, an ID, a profile directory and a label shown.
constexpr size_t kNameChars = 60;
constexpr size_t kIdChars = 64;
constexpr size_t kLabelChars = 40;
// The most of a value shown: the domain refuses a longer one, so nothing is
// cut (and a path the daemon lets through is shown exactly).
constexpr size_t kValueChars = 1000;
// The detail rows scroll beyond this height, and never take less (the dialog
// may then reach past a small window).
constexpr int kDetailsMaxHeight = 320;
constexpr int kDetailsMinHeight = 160;
// A detail's value is indented under its label, so no wrapped line of a value
// can look like a label of its own.
constexpr int kValueIndent = 16;
constexpr int kMinWidth = 480;
// How far below the top of the agent's window the dialog sits (under its
// toolbar).
constexpr int kTopOffset = 90;

constexpr char16_t kLri = 0x2066;  // LEFT-TO-RIGHT ISOLATE
constexpr char16_t kFsi = 0x2068;  // FIRST STRONG ISOLATE
constexpr char16_t kPdi = 0x2069;  // POP DIRECTIONAL ISOLATE
constexpr char16_t kLrm = 0x200E;  // LEFT-TO-RIGHT MARK

constexpr char16_t kTitle[] = u"에이전트가 확인을 요청합니다";
constexpr char16_t kApproveLabel[] = u"허용";
constexpr char16_t kDenyLabel[] = u"거부";
constexpr char16_t kAllowForSessionLabel[] = u"이 세션에서 계속 허용";
constexpr char16_t kFooter[] = u"60초 안에 답하지 않으면 거부됩니다";

std::string DirectoryOf(BrowserWindowInterface* browser) {
  Profile* profile = browser ? browser->GetProfile() : nullptr;
  return profile ? profile->GetBaseName().AsUTF8Unsafe() : std::string();
}

// The profile directory of the app's key window ("sheet:<dir>" when a sheet
// attached to that window is key; this dialog is none), "none" when there is
// no key window, "other" when it is no browser window's (this dialog itself).
std::string KeyWindowProfile() {
  if (!platform::HasKeyWindow()) {
    return "none";
  }
  for (BrowserWindowInterface* browser : GetAllBrowserWindowInterfaces()) {
    ui::BaseWindow* window = browser->GetWindow();
    if (!window) {
      continue;
    }
    switch (platform::KeyRelationOf(window->GetNativeWindow())) {
      case platform::KeyRelation::kKey:
        return DirectoryOf(browser);
      case platform::KeyRelation::kSheetIsKey:
        return "sheet:" + DirectoryOf(browser);
      case platform::KeyRelation::kNotKey:
        break;
    }
  }
  return "other";
}

// Text inside a sentence of the other language direction kept apart from it.
std::u16string Isolated(std::u16string text, char16_t opener) {
  return base::StrCat({std::u16string(1, opener), text, std::u16string(1, kPdi)});
}

// A path left to right, its directories in their order also when some are in
// a right-to-left script: a mark after each '/' keeps a separator between two
// right-to-left names from joining them into one reversed run.
std::u16string PathText(std::u16string path) {
  std::u16string out;
  out.reserve(path.size() * 2);
  for (char16_t c : path) {
    out.push_back(c);
    if (c == u'/') {
      out.push_back(kLrm);
    }
  }
  return out;
}

// A session's name as shown: DisplayText, and the characters the dialog's own
// lines use around names and IDs (« » ( ) ·) escaped too, so that a name
// cannot imitate them.
std::u16string NameText(const std::string& name) {
  std::u16string out;
  for (char16_t c : DisplayText(name, kNameChars)) {
    if (c == u'«' || c == u'»' || c == u'(' || c == u')' || c == u'·') {
      out += base::ASCIIToUTF16(base::StringPrintf("\\u{%x}", c));
    } else {
      out.push_back(c);
    }
  }
  return out;
}

bool IsPathLabel(std::string_view label) {
  return label == "파일" || label == "경로";
}

const std::string* DetailValue(const ConfirmRequest& request,
                               std::string_view label) {
  for (const auto& [l, v] : request.details) {
    if (l == label) {
      return &v;
    }
  }
  return nullptr;
}

// The second line: what the agent wants to do, in a sentence.
std::u16string Sentence(const ConfirmRequest& request) {
  auto shown = [](const std::string& v) {
    return Isolated(DisplayText(v, kValueChars), kLri);
  };
  if (request.action_class == "upload") {
    const size_t files = std::ranges::count_if(
        request.details, [](const auto& d) { return d.first == "파일"; });
    const std::u16string count =
        base::StrCat({u"파일 ", base::NumberToString16(files), u"개를 "});
    if (const std::string* page = DetailValue(request, "대상 페이지")) {
      return base::StrCat({count, shown(*page), u" 에 올리려 합니다"});
    }
    return base::StrCat({count, u"올리려 합니다"});
  }
  if (const std::string* origin = DetailValue(request, "출처")) {
    return base::StrCat({shown(*origin), u" 의 저장소를 바꾸려 합니다"});
  }
  if (const std::string* host = DetailValue(request, "호스트")) {
    return base::StrCat({shown(*host), u" 의 쿠키를 쓰려 합니다"});
  }
  return u"쿠키나 저장소를 바꾸려 합니다";
}

std::unique_ptr<views::Label> MakeLabel(const std::u16string& text,
                                        int width,
                                        int style,
                                        bool left_to_right,
                                        base::ListValue* lines,
                                        int indent = 0) {
  auto label = std::make_unique<views::Label>(
      text, views::style::CONTEXT_DIALOG_BODY_TEXT, style);
  label->SetMultiLine(true);
  // Long words (a path) wrap instead of being cut, and nothing is elided.
  label->SetAllowCharacterBreak(true);
  label->SetElideBehavior(gfx::NO_ELIDE);
  label->SetMaximumWidth(width - indent);
  label->SetHorizontalAlignment(gfx::ALIGN_LEFT);
  if (indent) {
    label->SetBorder(
        views::CreateEmptyBorder(gfx::Insets::TLBR(0, indent, 0, 0)));
  }
  if (left_to_right) {
    label->SetDirectionalityMode(gfx::DIRECTIONALITY_FORCE_LTR);
  }
  lines->Append(base::UTF16ToUTF8(text));
  return label;
}

// The dialog's contents: who asks, what, every detail row in order, and the
// time limit. `lines` gets each text shown, in order.
std::unique_ptr<views::View> MakeContents(const ConfirmRequest& request,
                                          const std::string& profile,
                                          int width,
                                          base::ListValue* lines) {
  views::LayoutProvider* layout = views::LayoutProvider::Get();
  const int related =
      layout->GetDistanceMetric(views::DISTANCE_RELATED_CONTROL_VERTICAL);
  const int unrelated =
      layout->GetDistanceMetric(views::DISTANCE_UNRELATED_CONTROL_VERTICAL);

  auto contents = std::make_unique<views::BoxLayoutView>();
  contents->SetOrientation(views::BoxLayout::Orientation::kVertical);
  contents->SetCrossAxisAlignment(
      views::BoxLayout::CrossAxisAlignment::kStretch);
  contents->SetBetweenChildSpacing(related);

  // First what the daemon and the browser know: the session's ID and the
  // window's profile. The name an agent chose comes after, on a line of its
  // own that says so, in another style, its « » ( ) · escaped.
  contents->AddChildView(MakeLabel(
      base::StrCat({u"세션 ",
                    Isolated(DisplayText(request.session_id, kIdChars), kLri),
                    u" · 프로필 ",
                    Isolated(DisplayText(profile, kNameChars), kLri)}),
      width, views::style::STYLE_EMPHASIZED, false, lines));
  contents->AddChildView(MakeLabel(
      base::StrCat({u"에이전트가 붙인 세션 이름: «",
                    Isolated(NameText(request.session_name), kFsi), u"»"}),
      width, views::style::STYLE_SECONDARY, false, lines));
  contents->AddChildView(MakeLabel(Sentence(request), width,
                                   views::style::STYLE_EMPHASIZED, false,
                                   lines));

  auto rows = std::make_unique<views::BoxLayoutView>();
  rows->SetOrientation(views::BoxLayout::Orientation::kVertical);
  rows->SetCrossAxisAlignment(views::BoxLayout::CrossAxisAlignment::kStretch);
  rows->SetBetweenChildSpacing(related / 2);
  // Room for a vertical scroll bar.
  const int row_width = width - 16;
  for (const auto& [label, value] : request.details) {
    rows->AddChildView(MakeLabel(DisplayText(label, kLabelChars), row_width,
                                 views::style::STYLE_SECONDARY, false, lines));
    std::u16string shown = DisplayText(value, kValueChars);
    if (IsPathLabel(label)) {
      shown = PathText(std::move(shown));
    }
    rows->AddChildView(MakeLabel(shown, row_width, views::style::STYLE_PRIMARY,
                                 true, lines, kValueIndent));
  }
  auto scroll = std::make_unique<views::ScrollView>();
  scroll->SetHorizontalScrollBarMode(
      views::ScrollView::ScrollBarMode::kDisabled);
  scroll->ClipHeightTo(
      std::min(kDetailsMinHeight, rows->GetHeightForWidth(row_width)),
      kDetailsMaxHeight);
  scroll->SetContents(std::move(rows));
  views::View* scroll_view = contents->AddChildView(std::move(scroll));
  scroll_view->SetProperty(views::kMarginsKey, gfx::Insets::VH(unrelated, 0));

  contents->AddChildView(MakeLabel(kFooter, width,
                                   views::style::STYLE_SECONDARY, false,
                                   lines));
  return contents;
}

}  // namespace

std::string_view AnswerName(ConfirmAnswer answer) {
  switch (answer) {
    case ConfirmAnswer::kApprove:
      return "approve";
    case ConfirmAnswer::kDeny:
      return "deny";
    case ConfirmAnswer::kAllowForSession:
      return "allowForSession";
  }
}

std::optional<ConfirmAnswer> AnswerOf(std::string_view name) {
  for (ConfirmAnswer a : {ConfirmAnswer::kApprove, ConfirmAnswer::kDeny,
                          ConfirmAnswer::kAllowForSession}) {
    if (AnswerName(a) == name) {
      return a;
    }
  }
  return std::nullopt;
}

ConfirmRequest::ConfirmRequest() = default;
ConfirmRequest::ConfirmRequest(ConfirmRequest&&) = default;
ConfirmRequest& ConfirmRequest::operator=(ConfirmRequest&&) = default;
ConfirmRequest::~ConfirmRequest() = default;

// static
std::unique_ptr<ConfirmDialog> ConfirmDialog::Show(
    BrowserWindowInterface* browser,
    ConfirmRequest request,
    Done done) {
  std::unique_ptr<ConfirmDialog> dialog(
      new ConfirmDialog(std::move(request), std::move(done)));
  if (!dialog->Open(browser)) {
    dialog->state_ = State::kClosed;
    return nullptr;
  }
  return dialog;
}

ConfirmDialog::ConfirmDialog(ConfirmRequest request, Done done)
    : request_(std::move(request)), done_(std::move(done)) {}

ConfirmDialog::~ConfirmDialog() {
  Close("destroyed");
}

bool ConfirmDialog::Open(BrowserWindowInterface* browser) {
  ui::BaseWindow* window = browser ? browser->GetWindow() : nullptr;
  if (!window) {
    return false;
  }
  profile_ = DirectoryOf(browser);
  views::LayoutProvider* layout = views::LayoutProvider::Get();
  const int width = std::max(
      kMinWidth,
      layout->GetDistanceMetric(views::DISTANCE_MODAL_DIALOG_PREFERRED_WIDTH));
  const gfx::Insets margins = layout->GetDialogInsetsForContentType(
      views::DialogContentType::kText, views::DialogContentType::kText);

  views::Widget* parent =
      views::Widget::GetWidgetForNativeWindow(window->GetNativeWindow());
  if (!parent) {
    return false;
  }
  delegate_ = std::make_unique<views::DialogDelegate>();
  delegate_->SetOwnershipOfNewWidget(
      views::Widget::InitParams::CLIENT_OWNS_WIDGET);
  // A child window of the agent's window, outside its web contents. Not
  // window-modal: on macOS that is a sheet, and AppKit makes a sheet the key
  // window as it appears, taking the keyboard from the person's window.
  delegate_->SetModalType(ui::mojom::ModalType::kNone);
  // (A person can still use the agent's window meanwhile.)
  delegate_->SetTitle(kTitle);
  delegate_->SetShowCloseButton(false);
  delegate_->set_fixed_width(width);
  delegate_->set_margins(margins);
  lines_.Append(base::UTF16ToUTF8(kTitle));
  delegate_->SetContentsView(MakeContents(
      request_, profile_, width - margins.width(), &lines_));

  delegate_->SetButtons(static_cast<int>(DialogButton::kOk) |
                        static_cast<int>(DialogButton::kCancel));
  delegate_->SetButtonLabel(DialogButton::kOk, kApproveLabel);
  delegate_->SetButtonLabel(DialogButton::kCancel, kDenyLabel);
  // Return and the initial focus are 거부, never 허용.
  delegate_->SetDefaultButton(static_cast<int>(DialogButton::kCancel));
  // The dialog never closes itself on a button: Press decides (the lock).
  delegate_->SetAcceptCallbackWithClose(base::BindRepeating(
      [](ConfirmDialog* d) {
        d->Press(ConfirmAnswer::kApprove, "button");
        return false;
      },
      base::Unretained(this)));
  // The cancel button, Return (the default button) and Esc.
  delegate_->SetCancelCallbackWithClose(base::BindRepeating(
      [](ConfirmDialog* d) {
        d->Press(ConfirmAnswer::kDeny, "button");
        return false;
      },
      base::Unretained(this)));
  buttons_.Append(base::UTF16ToUTF8(kApproveLabel));
  buttons_.Append(base::UTF16ToUTF8(kDenyLabel));
  if (request_.offer_allow_for_session) {
    allow_for_session_button_ =
        delegate_->SetExtraView(std::make_unique<views::MdTextButton>(
            base::BindRepeating(&ConfirmDialog::OnAllowForSessionPressed,
                                base::Unretained(this)),
            kAllowForSessionLabel));
    buttons_.Append(base::UTF16ToUTF8(kAllowForSessionLabel));
  }
  // Locked until Unlock().
  delegate_->SetButtonEnabled(DialogButton::kOk, false);
  delegate_->SetButtonEnabled(DialogButton::kCancel, false);
  if (allow_for_session_button_) {
    allow_for_session_button_->SetEnabled(false);
  }

  const bool hooks = test_hooks::Enabled();
  if (hooks) {
    focus_.Set("keyWindowProfileBefore", KeyWindowProfile());
    focus_.Set("appActiveBefore", platform::AppIsActive());
  }
  widget_.reset(views::DialogDelegate::CreateDialogWidget(
      delegate_.get(), gfx::NativeWindow(), parent->GetNativeView()));
  if (!widget_) {
    return false;
  }
  widget_observation_.Observe(widget_.get());
  // Keys and clicks on the dialog during the lock prolong it.
  lock_input_handler_ = std::make_unique<LockInputHandler>(this);
  widget_->GetRootView()->AddPreTargetHandler(lock_input_handler_.get());
  // Centered across the top of the agent's window, as a browser dialog; at
  // its full size also over a smaller window.
  const gfx::Rect over = parent->GetWindowBoundsInScreen();
  const gfx::Size size = widget_->GetRootView()->GetPreferredSize({});
  widget_->SetBounds(gfx::Rect(
      over.x() + (over.width() - size.width()) / 2,
      over.y() + std::max(0, std::min(kTopOffset,
                                      over.height() - size.height())),
      size.width(), size.height()));
  // Never Show(): that makes the dialog the key window and activates the app.
  // Shown inactive, a child window is ordered above its parent and nothing
  // else.
  widget_->ShowInactive();
  shown_at_ = base::TimeTicks::Now();
  allow_for_session_protector_.VisibilityChanged(true);
  // A picture-in-picture window over the dialog: no answer counts meanwhile.
  occlusion_observation_ =
      std::make_unique<ScopedPictureInPictureOcclusionObservation>(this);
  occlusion_observation_->Observe(widget_.get());
  const bool attention = platform::RequestAttentionIfInactive();
  unlock_timer_.Start(FROM_HERE, kLock,
                      base::BindOnce(&ConfirmDialog::Unlock,
                                     base::Unretained(this)));

  if (hooks) {
    focus_.Set("keyWindowProfileAt0", KeyWindowProfile());
    focus_.Set("appActiveAt0", platform::AppIsActive());
    focus_.Set("attention", attention);
    sample_timer_.Start(FROM_HERE, kFocusSample,
                        base::BindOnce(&ConfirmDialog::SampleFocus,
                                       base::Unretained(this)));
    base::DictValue shown;
    shown.Set("kind", "confirm_shown");
    shown.Set("confirmId", request_.confirm_id);
    shown.Set("profile", profile_);
    test_hooks::LogDialog(shown);
    std::optional<std::string> hook = test_hooks::AnswerFor(request_.session_name);
    std::optional<ConfirmAnswer> answer =
        hook ? AnswerOf(*hook) : std::nullopt;
    // Only a button the dialog has.
    if (answer == ConfirmAnswer::kAllowForSession &&
        !request_.offer_allow_for_session) {
      answer = std::nullopt;
    }
    if (answer) {
      hook_timer_.Start(FROM_HERE, test_hooks::AnswerAfter(),
                        base::BindOnce(&ConfirmDialog::Press,
                                       base::Unretained(this), *answer,
                                       "hook"));
    }
    // Clicks a person might make on the dialog, through the widget's own
    // event path.
    for (base::TimeDelta at : test_hooks::InputAt()) {
      auto timer = std::make_unique<base::OneShotTimer>();
      timer->Start(FROM_HERE, at,
                   base::BindOnce(&ConfirmDialog::SendTestClick,
                                  base::Unretained(this)));
      input_timers_.push_back(std::move(timer));
    }
  }
  return true;
}

void ConfirmDialog::LockInputHandler::OnKeyEvent(ui::KeyEvent* event) {
  if (event->type() == ui::EventType::kKeyPressed) {
    dialog_->OnInputDuringLock();
  }
}

void ConfirmDialog::LockInputHandler::OnMouseEvent(ui::MouseEvent* event) {
  if (event->type() == ui::EventType::kMousePressed) {
    dialog_->OnInputDuringLock();
  }
}

void ConfirmDialog::LockInputHandler::OnGestureEvent(
    ui::GestureEvent* event) {
  if (event->type() == ui::EventType::kGestureTapDown) {
    dialog_->OnInputDuringLock();
  }
}

void ConfirmDialog::OnInputDuringLock() {
  if (state_ != State::kOpen || !locked_) {
    return;
  }
  ++lock_inputs_;
  // Unlocked once there has been no input for kQuiet, and not before kLock.
  const base::TimeDelta left =
      std::max(kQuiet, kLock - (base::TimeTicks::Now() - shown_at_));
  unlock_timer_.Start(FROM_HERE, left,
                      base::BindOnce(&ConfirmDialog::Unlock,
                                     base::Unretained(this)));
}

void ConfirmDialog::SendTestClick() {
  if (!widget_ || state_ != State::kOpen) {
    return;
  }
  // A click at the dialog's top left corner (its frame, no button), as the
  // window system hands one to the widget.
  const gfx::Point at(2, 2);
  ui::MouseEvent press(ui::EventType::kMousePressed, at, at,
                       base::TimeTicks::Now(), ui::EF_LEFT_MOUSE_BUTTON,
                       ui::EF_LEFT_MOUSE_BUTTON);
  widget_->OnMouseEvent(&press);
  if (!widget_ || state_ != State::kOpen) {
    return;
  }
  ui::MouseEvent release(ui::EventType::kMouseReleased, at, at,
                         base::TimeTicks::Now(), ui::EF_LEFT_MOUSE_BUTTON,
                         ui::EF_LEFT_MOUSE_BUTTON);
  widget_->OnMouseEvent(&release);
}

void ConfirmDialog::OnAllowForSessionPressed(const ui::Event& event) {
  // As the dialog's own buttons: not a click too soon after it showed or
  // after another one.
  if (allow_for_session_protector_.IsPossiblyUnintendedInteraction(
          event, /*allow_key_events=*/true)) {
    Ignored("unintended");
    return;
  }
  Press(ConfirmAnswer::kAllowForSession, "button");
}

void ConfirmDialog::OnOcclusionStateChanged(bool occluded) {
  if (occluded_ && !occluded) {
    // Uncovered: as just unlocked.
    unlocked_at_ = base::TimeTicks::Now();
    if (views::DialogClientView* client = delegate_->GetDialogClientView()) {
      client->TriggerInputProtection();
    }
    allow_for_session_protector_.MaybeUpdateViewProtectedTimeStamp(true);
  }
  occluded_ = occluded;
}

void ConfirmDialog::Ignored(std::string_view why) {
  ignored_.Append(why);
}

void ConfirmDialog::Press(ConfirmAnswer answer, std::string_view by) {
  if (state_ != State::kOpen) {
    return;
  }
  if (locked_) {
    Ignored("locked");
    return;
  }
  if (base::TimeTicks::Now() - unlocked_at_ < kAfterUnlock) {
    Ignored("just_unlocked");
    return;
  }
  if (occluded_) {
    Ignored("occluded");
    return;
  }
  Finish(answer, by, "answered");
}

void ConfirmDialog::Deny(std::string_view why) {
  Finish(ConfirmAnswer::kDeny, "", why);
}

void ConfirmDialog::Finish(ConfirmAnswer answer,
                           std::string_view by,
                           std::string_view why) {
  if (state_ != State::kOpen) {
    return;
  }
  state_ = State::kAnswered;
  StopTimers();
  Log(answer, by, why);
  // Not from inside the widget's own event handling: `done` may delete this.
  base::SingleThreadTaskRunner::GetCurrentDefault()->PostTask(
      FROM_HERE, base::BindOnce(&ConfirmDialog::RunDone,
                                weak_factory_.GetWeakPtr(), answer));
}

void ConfirmDialog::RunDone(ConfirmAnswer answer) {
  DestroyWidget();
  // May delete this.
  std::move(done_).Run(answer);
}

void ConfirmDialog::Close(std::string_view why) {
  weak_factory_.InvalidateWeakPtrs();
  if (state_ == State::kOpen) {
    Log(std::nullopt, "", why);
  }
  state_ = State::kClosed;
  StopTimers();
  done_.Reset();
  DestroyWidget();
}

void ConfirmDialog::StopTimers() {
  unlock_timer_.Stop();
  sample_timer_.Stop();
  hook_timer_.Stop();
  input_timers_.clear();
}

void ConfirmDialog::DestroyWidget() {
  if (widget_ && lock_input_handler_) {
    widget_->GetRootView()->RemovePreTargetHandler(lock_input_handler_.get());
  }
  lock_input_handler_.reset();
  occlusion_observation_.reset();
  widget_observation_.Reset();
  allow_for_session_button_ = nullptr;
  widget_.reset();
}

void ConfirmDialog::OnWidgetClosing(views::Widget* widget) {
  // Its window closing closes it.
  Deny("window_closed");
}

void ConfirmDialog::OnWidgetDestroying(views::Widget* widget) {
  Deny("window_closed");
  if (lock_input_handler_) {
    widget->GetRootView()->RemovePreTargetHandler(lock_input_handler_.get());
    lock_input_handler_.reset();
  }
  widget_observation_.Reset();
  allow_for_session_button_ = nullptr;
}

void ConfirmDialog::Unlock() {
  if (state_ != State::kOpen || !delegate_) {
    return;
  }
  locked_ = false;
  unlocked_at_ = base::TimeTicks::Now();
  // The dialog's own click protection starts again from now.
  if (views::DialogClientView* client = delegate_->GetDialogClientView()) {
    client->TriggerInputProtection();
  }
  allow_for_session_protector_.MaybeUpdateViewProtectedTimeStamp(true);
  delegate_->SetButtonEnabled(DialogButton::kOk, true);
  delegate_->SetButtonEnabled(DialogButton::kCancel, true);
  if (allow_for_session_button_) {
    allow_for_session_button_->SetEnabled(true);
  }
}

void ConfirmDialog::SampleFocus() {
  focus_.Set("keyWindowProfileAt200ms", KeyWindowProfile());
  focus_.Set("appActiveAt200ms", platform::AppIsActive());
}

void ConfirmDialog::Log(std::optional<ConfirmAnswer> answer,
                        std::string_view by,
                        std::string_view why) {
  if (!test_hooks::Enabled()) {
    return;
  }
  base::DictValue entry;
  entry.Set("kind", "confirm");
  entry.Set("confirmId", request_.confirm_id);
  entry.Set("sessionId", request_.session_id);
  entry.Set("profile", profile_);
  entry.Set("lines", lines_.Clone());
  entry.Set("buttons", buttons_.Clone());
  entry.Set("defaultButton", base::UTF16ToUTF8(kDenyLabel));
  entry.Merge(focus_.Clone());
  if (answer) {
    entry.Set("pressed", AnswerName(*answer));
  } else {
    entry.Set("pressed", base::Value());
  }
  if (!by.empty()) {
    entry.Set("by", by);
  }
  entry.Set("closed", why);
  if (!ignored_.empty()) {
    entry.Set("ignored", ignored_.front().Clone());
    entry.Set("ignoredAll", ignored_.Clone());
  }
  entry.Set("lockInputs", lock_inputs_);
  test_hooks::LogDialog(entry);
}

}  // namespace pegboard
