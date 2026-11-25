// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/devtools/pegboard_handler.h"

#include <algorithm>
#include <map>
#include <set>
#include <string_view>
#include <utility>
#include <vector>

#include "base/callback_list.h"
#include "base/functional/bind.h"
#include "base/memory/raw_ptr.h"
#include "base/no_destructor.h"
#include "base/scoped_observation.h"
#include "base/strings/strcat.h"
#include "base/strings/string_util.h"
#include "base/strings/utf_string_conversions.h"
#include "base/task/single_thread_task_runner.h"
#include "base/time/time.h"
#include "base/timer/timer.h"
#include "base/types/expected.h"
#include "components/sessions/core/session_id.h"
#include "chrome/browser/browser_process.h"
#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/profiles/profile_attributes_entry.h"
#include "chrome/browser/profiles/profile_attributes_storage.h"
#include "chrome/browser/tab_group_sync/tab_group_sync_service_factory.h"
#include "chrome/browser/tab_list/tab_list_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "chrome/browser/ui/browser_window/public/desktop_browser_window_capabilities.h"
#include "chrome/browser/ui/navigator/browser_navigator.h"
#include "chrome/browser/ui/navigator/browser_navigator_params.h"
#include "components/saved_tab_groups/public/tab_group_sync_service.h"
#include "components/tab_groups/tab_group_color.h"
#include "components/tab_groups/tab_group_id.h"
#include "components/tab_groups/tab_group_visual_data.h"
#include "components/tabs/public/tab_interface.h"
#include "content/public/browser/devtools_agent_host.h"
#include "content/public/browser/devtools_agent_host_client.h"
#include "content/public/browser/devtools_agent_host_client_channel.h"
#include "content/public/browser/web_contents.h"
#include "pegboard/common/agent_profile.h"
#include "pegboard/ui/confirm_dialog.h"
#include "pegboard/ui/display_text.h"
#include "ui/base/page_transition_types.h"
#include "ui/base/window_open_disposition.h"
#include "ui/gfx/range/range.h"
#include "url/gurl.h"
#include "url/url_constants.h"

namespace pegboard {

namespace {

using protocol::Response;

// The session that owns the domain, if any (Pegboard.claim).
PegboardHandler* g_owner = nullptr;

// The names openProfileWindow makes a directory of: "PegboardAgent", or
// "PegboardAgent-" and lowercase ASCII letters, digits, '-' and '_' (no
// separator, no ".."). Lowercase only, so two agent names never differ only in
// case: on a case-insensitive file system they would be one directory, and two
// creations at once could both pass the collision check below.
bool IsCreatableAgentDirectoryName(std::string_view dir) {
  if (dir == kAgentDirectory) {
    return true;
  }
  if (!base::StartsWith(dir, kAgentDirectoryPrefix) || dir.size() > 128) {
    return false;
  }
  return std::ranges::all_of(
      dir.substr(kAgentDirectoryPrefix.size()), [](char c) {
        return base::IsAsciiLower(c) || base::IsAsciiDigit(c) || c == '-' ||
               c == '_';
      });
}

std::string DirectoryOf(const Profile* profile) {
  return profile->GetBaseName().AsUTF8Unsafe();
}

// Whether a profile the browser knows (loaded, or in the profile list) has a
// directory named `dir` apart from case. On a case-insensitive file system
// (macOS by default) that is the same directory under another name: a
// person's "pegboardagent" would be opened as the agent's "PegboardAgent".
bool CollidesApartFromCase(ProfileManager* manager, std::string_view dir) {
  auto collides = [dir](const base::FilePath& path) {
    const std::string name = path.BaseName().AsUTF8Unsafe();
    return name != dir && base::EqualsCaseInsensitiveASCII(name, dir);
  };
  for (Profile* profile : manager->GetLoadedProfiles()) {
    if (collides(profile->GetPath())) {
      return true;
    }
  }
  for (ProfileAttributesEntry* entry :
       manager->GetProfileAttributesStorage().GetAllProfilesAttributes()) {
    if (collides(entry->GetPath())) {
      return true;
    }
  }
  return false;
}

// The profiles getProfiles lists: a regular profile of the user data
// directory, not incognito, guest or system.
bool IsListedProfile(const Profile* profile) {
  ProfileManager* manager = g_browser_process->profile_manager();
  return manager && profile && !profile->IsOffTheRecord() &&
         !profile->IsSystemProfile() && !profile->IsGuestSession() &&
         profile->GetPath().DirName() == manager->user_data_dir();
}

bool IsAgentProfile(const Profile* profile) {
  return IsListedProfile(profile) && IsAgentDirectoryName(DirectoryOf(profile));
}

// The agent profile whose browser context is `id`; nullptr for any other
// context (a person's, an incognito one, one nobody knows).
Profile* AgentProfileOf(const std::string& id) {
  ProfileManager* manager = g_browser_process->profile_manager();
  if (!manager) {
    return nullptr;
  }
  for (Profile* profile : manager->GetLoadedProfiles()) {
    if (profile->UniqueId() == id) {
      return IsAgentProfile(profile) ? profile : nullptr;
    }
  }
  return nullptr;
}

// Group IDs on the wire: small integers handed out here, for the life of the
// browser process.
class Groups {
 public:
  int IdOf(const tab_groups::TabGroupId& group) {
    auto it = ids_.find(group);
    if (it != ids_.end()) {
      return it->second;
    }
    const int id = next_++;
    ids_.emplace(group, id);
    groups_.emplace(id, group);
    return id;
  }

  std::optional<tab_groups::TabGroupId> GroupOf(int id) const {
    auto it = groups_.find(id);
    if (it == groups_.end()) {
      return std::nullopt;
    }
    return it->second;
  }

 private:
  int next_ = 1;
  std::map<tab_groups::TabGroupId, int> ids_;
  std::map<int, tab_groups::TabGroupId> groups_;
};

Groups& GetGroups() {
  static base::NoDestructor<Groups> groups;
  return *groups;
}

std::optional<tab_groups::TabGroupColorId> ColorOf(std::string_view name) {
  using tab_groups::TabGroupColorId;
  static constexpr std::pair<std::string_view, TabGroupColorId> kColors[] = {
      {"grey", TabGroupColorId::kGrey},
      {"blue", TabGroupColorId::kBlue},
      {"red", TabGroupColorId::kRed},
      {"yellow", TabGroupColorId::kYellow},
      {"green", TabGroupColorId::kGreen},
      {"pink", TabGroupColorId::kPink},
      {"purple", TabGroupColorId::kPurple},
      {"cyan", TabGroupColorId::kCyan},
      {"orange", TabGroupColorId::kOrange},
  };
  for (const auto& [color_name, color] : kColors) {
    if (name == color_name) {
      return color;
    }
  }
  return std::nullopt;
}

// Only exactly about:blank and absolute http:, https: and data: URLs (as the
// bridge extension's allowedUrl).
std::optional<GURL> AllowedUrl(const std::string& url) {
  if (url == url::kAboutBlankURL) {
    return GURL(url::kAboutBlankURL);
  }
  GURL gurl(url);
  if (!gurl.is_valid() ||
      !(gurl.SchemeIs(url::kHttpScheme) || gurl.SchemeIs(url::kHttpsScheme) ||
        gurl.SchemeIs(url::kDataScheme))) {
    return std::nullopt;
  }
  // Absolute as written: GURL would forgive leading spaces and controls.
  if (!base::StartsWith(url, base::StrCat({gurl.scheme(), ":"}),
                        base::CompareCase::INSENSITIVE_ASCII)) {
    return std::nullopt;
  }
  return gurl;
}

std::string TargetIdOf(content::WebContents* contents) {
  return content::DevToolsAgentHost::GetOrCreateFor(contents)->GetId();
}

bool IsNormalWindow(BrowserWindowInterface* window) {
  return window && window->GetType() == BrowserWindowInterface::TYPE_NORMAL;
}

// The tab whose page target is `target_id`, when it is a tab of an agent
// profile's normal window; nullptr for anything else.
tabs::TabInterface* AgentTabOf(const std::string& target_id) {
  scoped_refptr<content::DevToolsAgentHost> host =
      content::DevToolsAgentHost::GetForId(target_id);
  if (!host || host->GetType() != content::DevToolsAgentHost::kTypePage) {
    return nullptr;
  }
  content::WebContents* contents = host->GetWebContents();
  if (!contents) {
    return nullptr;
  }
  // The tab's own page target, not a frame's or a prerendered page's.
  if (content::DevToolsAgentHost::GetOrCreateFor(contents) != host) {
    return nullptr;
  }
  tabs::TabInterface* tab = tabs::TabInterface::MaybeGetFromContents(contents);
  if (!tab) {
    return nullptr;
  }
  BrowserWindowInterface* window = tab->GetBrowserWindowInterface();
  Profile* profile = Profile::FromBrowserContext(contents->GetBrowserContext());
  if (!IsNormalWindow(window) || window->GetProfile() != profile ||
      !IsAgentProfile(profile)) {
    return nullptr;
  }
  return tab;
}

Response NotAgentTab(const std::string& target_id) {
  return Response::ServerError("not an agent profile's tab: " + target_id);
}

Response NotAgentContext(const std::string& id) {
  return Response::ServerError("not an agent profile's browser context: " + id);
}

// The window that has `group` in its tab strip.
BrowserWindowInterface* WindowWithGroup(const tab_groups::TabGroupId& group) {
  for (BrowserWindowInterface* window : GetAllBrowserWindowInterfaces()) {
    TabListInterface* list = TabListInterface::From(window);
    if (list && list->ContainsTabGroup(group)) {
      return window;
    }
  }
  return nullptr;
}

// The window of the group with wire ID `group_id` and its group, if it is
// still in a tab strip.
std::pair<BrowserWindowInterface*, std::optional<tab_groups::TabGroupId>>
FindGroup(int group_id) {
  std::optional<tab_groups::TabGroupId> group = GetGroups().GroupOf(group_id);
  if (!group) {
    return {nullptr, std::nullopt};
  }
  BrowserWindowInterface* window = WindowWithGroup(*group);
  if (!window) {
    return {nullptr, std::nullopt};
  }
  return {window, group};
}

// The most recently active normal window of `profile` that is not closing.
BrowserWindowInterface* TabbedWindowOf(Profile* profile) {
  BrowserWindowInterface* found = nullptr;
  ForEachCurrentBrowserWindowInterfaceOrderedByActivation(
      [profile, &found](BrowserWindowInterface* window) {
        if (window->GetProfile() == profile && IsNormalWindow(window) &&
            !window->capabilities()->IsAttemptingToCloseBrowser()) {
          found = window;
          return false;
        }
        return true;
      });
  return found;
}

// A new background tab of `url` in `window`, or in a new window of `profile`
// when `window` is null.
tabs::TabInterface* OpenTab(Profile* profile,
                            BrowserWindowInterface* window,
                            const GURL& url) {
  NavigateParams params(profile, url, ui::PAGE_TRANSITION_AUTO_TOPLEVEL);
  if (window) {
    params.disposition = WindowOpenDisposition::NEW_BACKGROUND_TAB;
    params.browser = window;
  } else {
    params.disposition = WindowOpenDisposition::NEW_WINDOW;
    // Never takes focus from the person's window (as the extension's
    // active:false tabs did not).
    params.window_action = NavigateParams::WindowAction::kShowWindowInactive;
  }
  Navigate(&params);
  if (!params.navigated_or_inserted_contents) {
    return nullptr;
  }
  tabs::TabInterface* tab = tabs::TabInterface::MaybeGetFromContents(
      params.navigated_or_inserted_contents);
  BrowserWindowInterface* opened_in =
      tab ? tab->GetBrowserWindowInterface() : nullptr;
  if (!IsNormalWindow(opened_in) || opened_in->GetProfile() != profile) {
    return nullptr;
  }
  return tab;
}

void CloseTab(tabs::TabInterface* tab) {
  BrowserWindowInterface* window = tab->GetBrowserWindowInterface();
  if (!window) {
    return;
  }
  if (TabListInterface* list = TabListInterface::From(window)) {
    list->CloseTab(tab->GetHandle());
  }
}

// How long openProfileWindow waits for the profile's saved-tab-group service.
// Well under pegboardd's 30 s request limit (cdp.rs REQUEST_TIMEOUT), which also
// covers making and loading the profile: the daemon must get this "did not
// start" answer rather than give up on the request first.
constexpr base::TimeDelta kTabGroupSyncWait = base::Seconds(15);

// Whether the saved-tab-group service of `profile` has started (or there is
// none). Until it has, a group made in the profile can lose its tabs: when the
// service starts it closes the tabs of a local group that its saved copy does
// not have (TabGroupSyncCoordinatorImpl::InitializeTabGroupSync). The service
// has no getter; AddObserver tells a late observer at once that it started.
bool IsTabGroupSyncReady(Profile* profile) {
  tab_groups::TabGroupSyncService* service =
      tab_groups::TabGroupSyncServiceFactory::GetForProfile(profile);
  if (!service) {
    return true;
  }
  class Probe : public tab_groups::TabGroupSyncService::Observer {
   public:
    void OnInitialized() override { started = true; }
    bool started = false;
  } probe;
  service->AddObserver(&probe);
  service->RemoveObserver(&probe);
  return probe.started;
}

Response NotStarted() {
  return Response::ServerError(
      "the agent profile's tab groups have not started yet (open it with "
      "openProfileWindow first)");
}

Response TabStripBusy() {
  return Response::ServerError("a tab strip of the profile is being dragged");
}

// A new group of `tab` named `title` in `color`; its wire ID.
std::optional<int> StartGroup(tabs::TabInterface* tab,
                              const std::string& title,
                              tab_groups::TabGroupColorId color) {
  BrowserWindowInterface* window = tab->GetBrowserWindowInterface();
  TabListInterface* list = window ? TabListInterface::From(window) : nullptr;
  if (!list) {
    return std::nullopt;
  }
  std::optional<tab_groups::TabGroupId> group =
      list->CreateTabGroup({tab->GetHandle()});
  if (!group) {
    return std::nullopt;
  }
  list->SetTabGroupVisualData(
      *group, tab_groups::TabGroupVisualData(base::UTF8ToUTF16(title), color));
  return GetGroups().IdOf(*group);
}

// The agent tabs of `target_ids`, or the first ID that is not one.
base::expected<std::vector<tabs::TabHandle>, std::string> AgentTabsOf(
    const std::vector<std::string>& target_ids) {
  std::vector<tabs::TabHandle> tabs;
  std::set<std::string> seen;
  for (const std::string& id : target_ids) {
    if (!seen.insert(id).second) {
      continue;
    }
    tabs::TabInterface* tab = AgentTabOf(id);
    if (!tab) {
      return base::unexpected(id);
    }
    tabs.push_back(tab->GetHandle());
  }
  return tabs;
}

bool CanEdit(const std::vector<tabs::TabHandle>& tabs) {
  for (const tabs::TabHandle& handle : tabs) {
    tabs::TabInterface* tab = handle.Get();
    if (!tab) {
      continue;
    }
    BrowserWindowInterface* window = tab->GetBrowserWindowInterface();
    if (!window || !TabListInterface::CanEditTabList(*window->GetProfile())) {
      return false;
    }
  }
  return true;
}


// Limits of requestConfirmation (design 4.5).
constexpr size_t kMaxConfirmations = 16;
constexpr size_t kMaxDetails = 40;
constexpr size_t kMaxLabelChars = 40;
constexpr size_t kMaxValueChars = 1000;
constexpr size_t kMaxSessionIdChars = 64;
// A dialog closes on its own (without an answer) this long after it was asked
// for, should the daemon never cancel it. Longer than the daemon's 60 s.
constexpr base::TimeDelta kConfirmationSafetyTime = base::Seconds(75);

// 128 bits as the daemon writes them: 32 lowercase hexadecimal digits.
bool IsConfirmId(std::string_view id) {
  return id.size() == 32 && std::ranges::all_of(id, [](char c) {
           return base::IsAsciiDigit(c) || (c >= 'a' && c <= 'f');
         });
}

// "s_" and the like: ASCII letters, digits, '_' and '-'.
bool IsSessionId(std::string_view id) {
  return !id.empty() && id.size() <= kMaxSessionIdChars &&
         std::ranges::all_of(id, [](char c) {
           return base::IsAsciiAlphaNumeric(c) || c == '_' || c == '-';
         });
}

// The normal window of the agent profile whose context is `context` and
// whose session ID is `id`, unless it is closing. (By IDs: the profile may
// have gone meanwhile.)
BrowserWindowInterface* AgentWindowWithId(const SessionID& id,
                                          const std::string& context) {
  Profile* profile = AgentProfileOf(context);
  if (!profile) {
    return nullptr;
  }
  for (BrowserWindowInterface* window : GetAllBrowserWindowInterfaces()) {
    if (window->GetSessionID() == id && IsNormalWindow(window) &&
        window->GetProfile() == profile &&
        !window->capabilities()->IsAttemptingToCloseBrowser()) {
      return window;
    }
  }
  return nullptr;
}

}  // namespace

// The owner's open confirmations: one dialog at a time per window (two
// dialogs over one window would cover each other), the others of that window
// waiting in the order they were asked for. Each ends once: with
// an answer (answered_ runs, the event), or closed without one (cancelled, its
// safety time, the owner going).
class Confirmations {
 public:
  using Answered =
      base::RepeatingCallback<void(const std::string&, ConfirmAnswer)>;

  explicit Confirmations(Answered answered) : answered_(std::move(answered)) {}
  Confirmations(const Confirmations&) = delete;
  Confirmations& operator=(const Confirmations&) = delete;
  // Closes every dialog without an answer.
  ~Confirmations() {
    for (auto& [id, entry] : entries_) {
      if (entry->dialog) {
        entry->dialog->Close("owner_gone");
      }
    }
  }

  bool Has(const std::string& id) const { return entries_.contains(id); }
  size_t size() const { return entries_.size(); }

  // Shows `request` on `window` (an agent window of `profile`), or queues it
  // behind the dialog up there. `tabs` are the session's tabs: one of them
  // closing, or leaving for another window, denies. False when it could not
  // be shown.
  bool Add(ConfirmRequest request,
           BrowserWindowInterface* window,
           Profile* profile,
           const std::vector<tabs::TabInterface*>& tabs) {
    const std::string id = request.confirm_id;
    auto entry = std::make_unique<Entry>();
    entry->seq = next_seq_++;
    entry->window = window->GetSessionID();
    entry->context = profile->UniqueId();
    entry->request = std::move(request);
    entry->safety.Start(FROM_HERE, kConfirmationSafetyTime,
                        base::BindOnce(&Confirmations::Expire,
                                       base::Unretained(this), id));
    for (tabs::TabInterface* tab : tabs) {
      entry->tab_watches.push_back(tab->RegisterWillDetach(
          base::BindRepeating(&Confirmations::TabWillDetach,
                              base::Unretained(this), id)));
    }
    Entry* added = entries_.emplace(id, std::move(entry)).first->second.get();
    if (!ShownIn(added->window)) {
      if (!ShowEntry(id, added, window)) {
        entries_.erase(id);
        return false;
      }
    }
    return true;
  }

  // Closes it without an answer; nothing for one that is gone.
  void Cancel(const std::string& id) { Remove(id, "cancelled"); }

 private:
  struct Entry {
    uint64_t seq = 0;
    SessionID window = SessionID::InvalidValue();
    // The profile's browser context ID.
    std::string context;
    ConfirmRequest request;
    std::unique_ptr<ConfirmDialog> dialog;
    base::OneShotTimer safety;
    std::vector<base::CallbackListSubscription> tab_watches;
  };

  bool ShownIn(const SessionID& window) const {
    return std::ranges::any_of(entries_, [&window](const auto& e) {
      return e.second->window == window && e.second->dialog;
    });
  }

  bool ShowEntry(const std::string& id,
                 Entry* entry,
                 BrowserWindowInterface* window) {
    ConfirmRequest copy;
    copy.confirm_id = entry->request.confirm_id;
    copy.session_id = entry->request.session_id;
    copy.session_name = entry->request.session_name;
    copy.profile_directory = entry->request.profile_directory;
    copy.action_class = entry->request.action_class;
    copy.details = entry->request.details;
    copy.offer_allow_for_session = entry->request.offer_allow_for_session;
    entry->dialog = ConfirmDialog::Show(
        window, std::move(copy),
        base::BindOnce(&Confirmations::Done, weak_factory_.GetWeakPtr(), id));
    return entry->dialog != nullptr;
  }

  // The next one asked for in `window`, once nothing is shown there. One whose
  // window has gone is denied (its window closed).
  void ShowNext(const SessionID& window) {
    while (!ShownIn(window)) {
      Entry* next = nullptr;
      std::string next_id;
      for (auto& [id, entry] : entries_) {
        if (entry->window == window && (!next || entry->seq < next->seq)) {
          next = entry.get();
          next_id = id;
        }
      }
      if (!next) {
        return;
      }
      BrowserWindowInterface* w = AgentWindowWithId(window, next->context);
      if (w && ShowEntry(next_id, next, w)) {
        return;
      }
      entries_.erase(next_id);
      answered_.Run(next_id, ConfirmAnswer::kDeny);
    }
  }

  // The dialog's answer (in a task of its own).
  void Done(std::string id, ConfirmAnswer answer) {
    auto it = entries_.find(id);
    if (it == entries_.end()) {
      return;
    }
    const SessionID window = it->second->window;
    entries_.erase(it);
    answered_.Run(id, answer);
    ShowNext(window);
  }

  // A tab of the session closing, or dragged to another window (the dialog
  // would no longer be over the session's tabs): deny.
  void TabWillDetach(std::string id,
                     tabs::TabInterface* tab,
                     tabs::TabInterface::DetachReason reason) {
    const char* why = reason == tabs::TabInterface::DetachReason::kDelete
                          ? "tab_closed"
                          : "tab_moved";
    auto it = entries_.find(id);
    if (it == entries_.end()) {
      return;
    }
    if (it->second->dialog) {
      // Answers kDeny through Done.
      it->second->dialog->Deny(why);
      return;
    }
    entries_.erase(it);
    answered_.Run(id, ConfirmAnswer::kDeny);
  }

  void Expire(std::string id) { Remove(id, "safety_time"); }

  void Remove(const std::string& id, std::string_view why) {
    auto it = entries_.find(id);
    if (it == entries_.end()) {
      return;
    }
    const SessionID window = it->second->window;
    if (it->second->dialog) {
      it->second->dialog->Close(why);
    }
    entries_.erase(it);
    ShowNext(window);
  }

  Answered answered_;
  std::map<std::string, std::unique_ptr<Entry>> entries_;
  uint64_t next_seq_ = 1;
  base::WeakPtrFactory<Confirmations> weak_factory_{this};
};

// static
bool PegboardHandler::IsAvailable(
    content::DevToolsAgentHostClientChannel* channel) {
  content::DevToolsAgentHostClient* client = channel->GetClient();
  if (channel->GetAgentHost()->GetType() !=
          content::DevToolsAgentHost::kTypeBrowser ||
      !client->IsTrusted() || !client->AllowUnsafeOperations()) {
    return false;
  }
  // The unsafe clause above is what does the work in 156: only the pipe, the
  // sessions opened on it and a Target.exposeDevToolsProtocol binding that
  // inherited permissions answer true, and pegboard-cdp-domain.patch refuses
  // inheritPermissions. The DevTools front end (DevToolsUIBindings) also says
  // "DevTools" below and is stopped by that clause. The IsTrusted check and
  // the kind check here are defensive and redundant in 156 (no test can make
  // them matter): the pipe's own session says "RemoteDebugger", the sessions
  // opened on it with Target.attachToBrowserTarget (content's
  // TargetHandler::Session) "DevTools", and a binding "Other". content has no
  // type to test, so the kind is the name the client gives metrics.
  const std::string kind = client->GetTypeForMetrics();
  return kind == "RemoteDebugger" || kind == "DevTools";
}

PegboardHandler::PegboardHandler(protocol::UberDispatcher* dispatcher)
    : frontend_(std::make_unique<protocol::Pegboard::Frontend>(
          dispatcher->channel())) {
  protocol::Pegboard::Dispatcher::wire(dispatcher, this);
}

PegboardHandler::~PegboardHandler() {
  // The owner goes: its dialogs close, without an event.
  confirmations_.reset();
  // Before TabStripModelObserver's destructor, which wants no observation.
  tab_strip_tracker_.reset();
  if (g_owner == this) {
    g_owner = nullptr;
  }
}

bool PegboardHandler::IsOwner() const {
  return g_owner == this;
}

// static
Response PegboardHandler::NotFound(const char* method) {
  return Response::MethodNotFound(std::string("'Pegboard.") + method +
                                  "' wasn't found");
}

Response PegboardHandler::Claim() {
  if (IsOwner()) {
    return Response::Success();
  }
  if (g_owner) {
    return Response::ServerError("already claimed");
  }
  g_owner = this;
  StartObserving();
  return Response::Success();
}

void PegboardHandler::StartObserving() {
  ProfileManager* manager = g_browser_process->profile_manager();
  if (manager) {
    profile_manager_observation_.Observe(manager);
    for (Profile* profile : manager->GetLoadedProfiles()) {
      if (IsListedProfile(profile)) {
        ObserveProfile(profile);
      }
    }
  }
  tab_strip_tracker_ = std::make_unique<BrowserTabStripTracker>(this, this);
  tab_strip_tracker_->Init();
}

void PegboardHandler::ObserveProfile(Profile* profile) {
  if (!profile_observations_.IsObservingSource(profile)) {
    profile_observations_.AddObservation(profile);
  }
}

Response PegboardHandler::GetProfiles(
    std::unique_ptr<protocol::Array<protocol::Pegboard::ProfileInfo>>*
        out_profiles) {
  if (!IsOwner()) {
    return NotFound("getProfiles");
  }
  auto profiles =
      std::make_unique<protocol::Array<protocol::Pegboard::ProfileInfo>>();
  if (ProfileManager* manager = g_browser_process->profile_manager()) {
    for (Profile* profile : manager->GetLoadedProfiles()) {
      if (!IsListedProfile(profile)) {
        continue;
      }
      profiles->push_back(protocol::Pegboard::ProfileInfo::Create()
                              .SetProfileDirectory(DirectoryOf(profile))
                              .SetBrowserContextId(profile->UniqueId())
                              .SetIsAgent(IsAgentProfile(profile))
                              .Build());
    }
  }
  *out_profiles = std::move(profiles);
  return Response::Success();
}

void PegboardHandler::OpenProfileWindow(
    const std::string& profile_directory,
    std::unique_ptr<OpenProfileWindowCallback> callback) {
  if (!IsOwner()) {
    callback->sendFailure(NotFound("openProfileWindow"));
    return;
  }
  if (!IsCreatableAgentDirectoryName(profile_directory)) {
    callback->sendFailure(Response::ServerError(
        "not an agent profile directory: " + profile_directory));
    return;
  }
  ProfileManager* manager = g_browser_process->profile_manager();
  if (!manager) {
    callback->sendFailure(Response::ServerError("no profile manager"));
    return;
  }
  if (CollidesApartFromCase(manager, profile_directory)) {
    callback->sendFailure(Response::ServerError(
        "another profile's directory differs only in case: " +
        profile_directory));
    return;
  }
  const base::FilePath path =
      manager->user_data_dir().AppendASCII(profile_directory);
  if (Profile* profile = manager->GetProfileByPath(path)) {
    OnProfileReady(profile_directory, std::move(callback), profile);
    return;
  }
  // Makes the profile when it does not exist. Nothing here shows first-run
  // UI: that comes only with a startup browser.
  manager->CreateProfileAsync(
      path, base::BindOnce(&PegboardHandler::OnProfileReady,
                           weak_factory_.GetWeakPtr(), profile_directory,
                           std::move(callback)));
}

void PegboardHandler::OnProfileReady(
    std::string profile_directory,
    std::unique_ptr<OpenProfileWindowCallback> callback,
    Profile* profile) {
  if (!profile || !IsAgentProfile(profile) ||
      DirectoryOf(profile) != profile_directory) {
    callback->sendFailure(Response::ServerError(
        "could not load the agent profile " + profile_directory));
    return;
  }
  if (!TabbedWindowOf(profile) &&
      !OpenTab(profile, nullptr, GURL(url::kAboutBlankURL))) {
    callback->sendFailure(Response::ServerError("could not open a window of " +
                                                profile_directory));
    return;
  }
  if (IsTabGroupSyncReady(profile)) {
    callback->sendSuccess(profile->UniqueId());
    return;
  }
  // A profile just loaded: answer once its saved-tab-group service started,
  // so the groups the daemon makes next keep their tabs.
  const int id = next_waiter_id_++;
  waiters_[id] = std::make_unique<TabGroupSyncWaiter>(
      tab_groups::TabGroupSyncServiceFactory::GetForProfile(profile),
      base::BindOnce(&PegboardHandler::OnTabGroupSyncStarted,
                     weak_factory_.GetWeakPtr(), id, profile->UniqueId(),
                     profile_directory, std::move(callback)));
}

void PegboardHandler::OnTabGroupSyncStarted(
    int waiter,
    std::string browser_context_id,
    std::string profile_directory,
    std::unique_ptr<OpenProfileWindowCallback> callback,
    bool started) {
  waiters_.erase(waiter);
  if (!started) {
    callback->sendFailure(Response::ServerError(
        "the tab groups of " + profile_directory + " did not start"));
    return;
  }
  callback->sendSuccess(browser_context_id);
}

TabGroupSyncWaiter::TabGroupSyncWaiter(tab_groups::TabGroupSyncService* service,
                                       base::OnceCallback<void(bool)> done)
    : done_(std::move(done)) {
  timer_.Start(FROM_HERE, kTabGroupSyncWait,
               base::BindOnce(&TabGroupSyncWaiter::Finish,
                              base::Unretained(this), false));
  observation_.Observe(service);
}

TabGroupSyncWaiter::~TabGroupSyncWaiter() = default;

void TabGroupSyncWaiter::OnInitialized() {
  Finish(true);
}

void TabGroupSyncWaiter::OnWillBeDestroyed() {
  Finish(false);
}

void TabGroupSyncWaiter::Finish(bool started) {
  if (!done_) {
    return;
  }
  observation_.Reset();
  timer_.Stop();
  // Posted: the service runs its own start-up (the coordinator's, which
  // closes tabs) in the same task, and the owner deletes this waiter.
  base::SingleThreadTaskRunner::GetCurrentDefault()->PostTask(
      FROM_HERE, base::BindOnce(std::move(done_), started));
}

Response PegboardHandler::CreateGroup(const std::string& browser_context_id,
                                      const std::string& title,
                                      const std::string& color,
                                      int* out_group_id,
                                      std::string* out_target_id) {
  if (!IsOwner()) {
    return NotFound("createGroup");
  }
  Profile* profile = AgentProfileOf(browser_context_id);
  if (!profile) {
    return NotAgentContext(browser_context_id);
  }
  std::optional<tab_groups::TabGroupColorId> group_color = ColorOf(color);
  if (!group_color) {
    return Response::ServerError("unknown group color: " + color);
  }
  if (!TabListInterface::CanEditTabList(*profile)) {
    return TabStripBusy();
  }
  if (!IsTabGroupSyncReady(profile)) {
    return NotStarted();
  }
  tabs::TabInterface* tab =
      OpenTab(profile, TabbedWindowOf(profile), GURL(url::kAboutBlankURL));
  if (!tab) {
    return Response::ServerError("could not open a tab");
  }
  std::optional<int> group = StartGroup(tab, title, *group_color);
  if (!group) {
    CloseTab(tab);
    return Response::ServerError("could not make a group");
  }
  *out_group_id = *group;
  *out_target_id = TargetIdOf(tab->GetContents());
  return Response::Success();
}

Response PegboardHandler::CreateTab(const std::string& browser_context_id,
                                    int group_id,
                                    const std::string& url,
                                    const std::string& title,
                                    const std::string& color,
                                    std::string* out_target_id,
                                    std::optional<int>* out_new_group_id) {
  if (!IsOwner()) {
    return NotFound("createTab");
  }
  Profile* profile = AgentProfileOf(browser_context_id);
  if (!profile) {
    return NotAgentContext(browser_context_id);
  }
  std::optional<GURL> gurl = AllowedUrl(url);
  if (!gurl) {
    return Response::ServerError("url not allowed: " + url);
  }
  if (!TabListInterface::CanEditTabList(*profile)) {
    return TabStripBusy();
  }
  if (!IsTabGroupSyncReady(profile)) {
    return NotStarted();
  }
  auto [window, group] = FindGroup(group_id);
  if (window) {
    if (window->GetProfile() != profile || !IsNormalWindow(window)) {
      return Response::ServerError("the group is not this profile's");
    }
    tabs::TabInterface* tab = OpenTab(profile, window, *gurl);
    if (!tab) {
      return Response::ServerError("could not open a tab");
    }
    TabListInterface* list = TabListInterface::From(window);
    if (tab->GetBrowserWindowInterface() != window ||
        !list->AddTabsToGroup(group, {tab->GetHandle()})) {
      CloseTab(tab);
      return Response::ServerError("could not add the tab to its group");
    }
    *out_target_id = TargetIdOf(tab->GetContents());
    return Response::Success();
  }
  // The group is gone (Chrome deletes a group with its last tab): the tab
  // starts a new one. Checked before any tab exists, so none is left over.
  std::optional<tab_groups::TabGroupColorId> group_color = ColorOf(color);
  if (!group_color) {
    return Response::ServerError("unknown group color: " + color);
  }
  tabs::TabInterface* tab = OpenTab(profile, TabbedWindowOf(profile), *gurl);
  if (!tab) {
    return Response::ServerError("could not open a tab");
  }
  std::optional<int> new_group = StartGroup(tab, title, *group_color);
  if (!new_group) {
    CloseTab(tab);
    return Response::ServerError("could not make a group");
  }
  *out_target_id = TargetIdOf(tab->GetContents());
  *out_new_group_id = *new_group;
  return Response::Success();
}

Response PegboardHandler::AddToGroup(int group_id,
                                     const std::string& target_id) {
  if (!IsOwner()) {
    return NotFound("addToGroup");
  }
  tabs::TabInterface* tab = AgentTabOf(target_id);
  if (!tab) {
    return NotAgentTab(target_id);
  }
  auto [window, group] = FindGroup(group_id);
  if (!window) {
    return Response::ServerError("no such group");
  }
  Profile* profile = tab->GetBrowserWindowInterface()->GetProfile();
  if (window->GetProfile() != profile || !IsNormalWindow(window)) {
    return Response::ServerError("the group is not the tab's profile's");
  }
  if (!TabListInterface::CanEditTabList(*profile)) {
    return TabStripBusy();
  }
  TabListInterface* list = TabListInterface::From(window);
  const tabs::TabHandle handle = tab->GetHandle();
  if (tab->GetBrowserWindowInterface() != window) {
    TabListInterface* from =
        TabListInterface::From(tab->GetBrowserWindowInterface());
    if (!from) {
      return Response::ServerError("could not move the tab to its group");
    }
    from->MoveTabToWindow(handle, window->GetSessionID(), list->GetTabCount());
    tab = handle.Get();
    if (!tab || tab->GetBrowserWindowInterface() != window) {
      return Response::ServerError("could not move the tab to its group");
    }
  }
  if (!list->AddTabsToGroup(group, {handle})) {
    return Response::ServerError("could not add the tab to the group");
  }
  return Response::Success();
}

Response PegboardHandler::GetGroupMembers(
    int group_id,
    std::unique_ptr<protocol::Array<std::string>>* out_target_ids) {
  if (!IsOwner()) {
    return NotFound("getGroupMembers");
  }
  auto ids = std::make_unique<protocol::Array<std::string>>();
  auto [window, group] = FindGroup(group_id);
  if (window) {
    if (!IsAgentProfile(window->GetProfile())) {
      return Response::ServerError("not an agent profile's group");
    }
    TabListInterface* list = TabListInterface::From(window);
    const gfx::Range range = list->GetTabGroupTabIndices(*group);
    for (size_t i = range.start(); i < range.end(); ++i) {
      if (tabs::TabInterface* tab = list->GetTab(static_cast<int>(i))) {
        ids->push_back(TargetIdOf(tab->GetContents()));
      }
    }
  }
  *out_target_ids = std::move(ids);
  return Response::Success();
}

Response PegboardHandler::CloseTargets(
    std::unique_ptr<protocol::Array<std::string>> target_ids) {
  if (!IsOwner()) {
    return NotFound("closeTargets");
  }
  // All or nothing: one tab that is not an agent's refuses the whole call.
  auto tabs = AgentTabsOf(*target_ids);
  if (!tabs.has_value()) {
    return NotAgentTab(tabs.error());
  }
  if (!CanEdit(*tabs)) {
    return TabStripBusy();
  }
  for (const tabs::TabHandle& handle : *tabs) {
    if (tabs::TabInterface* tab = handle.Get()) {
      CloseTab(tab);
    }
  }
  return Response::Success();
}

Response PegboardHandler::UngroupTargets(
    std::unique_ptr<protocol::Array<std::string>> target_ids) {
  if (!IsOwner()) {
    return NotFound("ungroupTargets");
  }
  auto tabs = AgentTabsOf(*target_ids);
  if (!tabs.has_value()) {
    return NotAgentTab(tabs.error());
  }
  if (!CanEdit(*tabs)) {
    return TabStripBusy();
  }
  for (const tabs::TabHandle& handle : *tabs) {
    tabs::TabInterface* tab = handle.Get();
    if (!tab || !tab->GetGroup() || !tab->GetBrowserWindowInterface()) {
      continue;
    }
    if (TabListInterface* list =
            TabListInterface::From(tab->GetBrowserWindowInterface())) {
      list->Ungroup({handle});
    }
  }
  return Response::Success();
}

Response PegboardHandler::Ungroup(int group_id) {
  if (!IsOwner()) {
    return NotFound("ungroup");
  }
  auto [window, group] = FindGroup(group_id);
  if (!window) {
    return Response::Success();
  }
  if (!IsAgentProfile(window->GetProfile())) {
    return Response::ServerError("not an agent profile's group");
  }
  if (!TabListInterface::CanEditTabList(*window->GetProfile())) {
    return TabStripBusy();
  }
  TabListInterface* list = TabListInterface::From(window);
  const gfx::Range range = list->GetTabGroupTabIndices(*group);
  std::set<tabs::TabHandle> members;
  for (size_t i = range.start(); i < range.end(); ++i) {
    if (tabs::TabInterface* tab = list->GetTab(static_cast<int>(i))) {
      members.insert(tab->GetHandle());
    }
  }
  if (!members.empty()) {
    list->Ungroup(members);
  }
  return Response::Success();
}

Response PegboardHandler::RequestConfirmation(
    const std::string& confirm_id,
    const std::string& browser_context_id,
    std::unique_ptr<protocol::Array<std::string>> target_ids,
    const std::string& session_id,
    const std::string& session_name,
    const std::string& profile_directory,
    const std::string& action_class,
    std::unique_ptr<protocol::Array<protocol::Pegboard::ConfirmationDetail>>
        details,
    bool offer_allow_for_session) {
  if (!IsOwner()) {
    return NotFound("requestConfirmation");
  }
  if (!confirmations_) {
    confirmations_ = std::make_unique<Confirmations>(base::BindRepeating(
        &PegboardHandler::OnConfirmationAnswered, base::Unretained(this)));
  }
  if (!IsConfirmId(confirm_id)) {
    return Response::ServerError(
        "confirmId is not 32 lowercase hexadecimal digits");
  }
  if (confirmations_->Has(confirm_id)) {
    return Response::ServerError("confirmation already open: " + confirm_id);
  }
  if (confirmations_->size() >= kMaxConfirmations) {
    return Response::ServerError("too many confirmations open");
  }
  if (action_class != "upload" && action_class != "state") {
    return Response::ServerError("unknown actionClass: " + action_class);
  }
  if (!IsSessionId(session_id)) {
    return Response::ServerError("malformed sessionId");
  }
  if (details->size() > kMaxDetails) {
    return Response::ServerError("too many details");
  }
  ConfirmRequest request;
  for (const std::unique_ptr<protocol::Pegboard::ConfirmationDetail>& d :
       *details) {
    std::string label = d->GetLabel();
    std::string value = d->GetValue();
    if (CharCount(label) > kMaxLabelChars) {
      return Response::ServerError("a detail's label is too long");
    }
    if (CharCount(value) > kMaxValueChars) {
      return Response::ServerError("a detail's value is too long");
    }
    request.details.emplace_back(std::move(label), std::move(value));
  }
  Profile* profile = AgentProfileOf(browser_context_id);
  if (!profile) {
    return NotAgentContext(browser_context_id);
  }
  if (profile_directory != DirectoryOf(profile)) {
    return Response::ServerError(
        "profileDirectory is not the browser context's: " + profile_directory);
  }
  // Every tab must be one of this agent profile's (fail closed: a tab that is
  // gone, a person's or another profile's refuses the whole request).
  std::vector<tabs::TabInterface*> tabs;
  for (const std::string& id : *target_ids) {
    tabs::TabInterface* tab = AgentTabOf(id);
    if (!tab || tab->GetBrowserWindowInterface()->GetProfile() != profile) {
      return Response::ServerError("not a tab of " + profile_directory + ": " +
                                   id);
    }
    tabs.push_back(tab);
  }
  BrowserWindowInterface* window = nullptr;
  for (tabs::TabInterface* tab : tabs) {
    BrowserWindowInterface* w = tab->GetBrowserWindowInterface();
    if (!w->capabilities()->IsAttemptingToCloseBrowser()) {
      window = w;
      break;
    }
  }
  if (!window) {
    window = TabbedWindowOf(profile);
  }
  if (!window || !IsNormalWindow(window) || window->GetProfile() != profile) {
    return Response::ServerError("no window");
  }
  request.confirm_id = confirm_id;
  request.session_id = session_id;
  request.session_name = session_name;
  request.profile_directory = profile_directory;
  request.action_class = action_class;
  request.offer_allow_for_session = offer_allow_for_session;
  if (!confirmations_->Add(std::move(request), window, profile, tabs)) {
    return Response::ServerError("could not show the dialog");
  }
  return Response::Success();
}

Response PegboardHandler::CancelConfirmation(const std::string& confirm_id) {
  if (!IsOwner()) {
    return NotFound("cancelConfirmation");
  }
  if (confirmations_) {
    confirmations_->Cancel(confirm_id);
  }
  return Response::Success();
}

void PegboardHandler::OnConfirmationAnswered(const std::string& confirm_id,
                                             ConfirmAnswer answer) {
  // Only the owner has confirmations, and this handler is its session's: the
  // event goes to that session alone.
  if (!IsOwner()) {
    return;
  }
  frontend_->ConfirmationAnswered(confirm_id, std::string(AnswerName(answer)));
}

void PegboardHandler::OnProfileAdded(Profile* profile) {
  if (!IsListedProfile(profile)) {
    return;
  }
  ObserveProfile(profile);
  frontend_->ProfileLoaded(DirectoryOf(profile), profile->UniqueId());
}

void PegboardHandler::OnProfileManagerDestroying() {
  profile_manager_observation_.Reset();
}

void PegboardHandler::OnProfileWillBeDestroyed(Profile* profile) {
  if (IsListedProfile(profile)) {
    frontend_->ProfileUnloaded(DirectoryOf(profile), profile->UniqueId());
  }
  if (profile_observations_.IsObservingSource(profile)) {
    profile_observations_.RemoveObservation(profile);
  }
}

bool PegboardHandler::ShouldTrackBrowser(BrowserWindowInterface* browser) {
  return IsNormalWindow(browser) && IsAgentProfile(browser->GetProfile());
}

void PegboardHandler::OnTabStripModelChanged(
    TabStripModel* tab_strip_model,
    const TabStripModelChange& change,
    const TabStripSelectionChange& selection) {
  // Not the state of the windows open when the domain was claimed.
  if (!tab_strip_tracker_ ||
      tab_strip_tracker_->is_processing_initial_browsers()) {
    return;
  }
  if (!selection.active_tab_changed() || !selection.new_contents) {
    return;
  }
  content::WebContents* contents = selection.new_contents;
  Profile* profile = Profile::FromBrowserContext(contents->GetBrowserContext());
  tabs::TabInterface* tab = tabs::TabInterface::MaybeGetFromContents(contents);
  BrowserWindowInterface* window =
      tab ? tab->GetBrowserWindowInterface() : nullptr;
  if (!IsAgentProfile(profile) || !IsNormalWindow(window) ||
      window->GetProfile() != profile) {
    return;
  }
  frontend_->TabActivated(profile->UniqueId(), TargetIdOf(contents),
                          base::Time::Now().InMillisecondsFSinceUnixEpoch());
}

}  // namespace pegboard
