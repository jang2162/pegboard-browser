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

#include "base/functional/bind.h"
#include "base/memory/raw_ptr.h"
#include "base/no_destructor.h"
#include "base/strings/strcat.h"
#include "base/strings/string_util.h"
#include "base/strings/utf_string_conversions.h"
#include "base/time/time.h"
#include "base/types/expected.h"
#include "chrome/browser/browser_process.h"
#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/tab_list/tab_list_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "chrome/browser/ui/browser_window/public/desktop_browser_window_capabilities.h"
#include "chrome/browser/ui/navigator/browser_navigator.h"
#include "chrome/browser/ui/navigator/browser_navigator_params.h"
#include "components/tab_groups/tab_group_color.h"
#include "components/tab_groups/tab_group_id.h"
#include "components/tab_groups/tab_group_visual_data.h"
#include "components/tabs/public/tab_interface.h"
#include "content/public/browser/devtools_agent_host.h"
#include "content/public/browser/devtools_agent_host_client.h"
#include "content/public/browser/devtools_agent_host_client_channel.h"
#include "content/public/browser/web_contents.h"
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

constexpr std::string_view kAgentDirectory = "PegboardAgent";
constexpr std::string_view kAgentDirectoryPrefix = "PegboardAgent-";

// The agent profile rule: a directory named exactly "PegboardAgent" or
// starting with "PegboardAgent-". Every other profile is a person's.
bool IsAgentDirectoryName(std::string_view dir) {
  return dir == kAgentDirectory || base::StartsWith(dir, kAgentDirectoryPrefix);
}

// The names openProfileWindow makes a directory of: agent names of ASCII
// letters, digits, '-' and '_' only (no separator, no "..").
bool IsCreatableAgentDirectoryName(std::string_view dir) {
  return IsAgentDirectoryName(dir) && dir.size() <= 128 &&
         std::ranges::all_of(dir, [](char c) {
           return base::IsAsciiAlphaNumeric(c) || c == '-' || c == '_';
         });
}

std::string DirectoryOf(const Profile* profile) {
  return profile->GetBaseName().AsUTF8Unsafe();
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
    params.window_action = NavigateParams::WindowAction::kShowWindow;
  }
  Navigate(&params);
  if (!params.navigated_or_inserted_contents) {
    return nullptr;
  }
  tabs::TabInterface* tab = tabs::TabInterface::MaybeGetFromContents(
      params.navigated_or_inserted_contents);
  if (!tab || !IsNormalWindow(tab->GetBrowserWindowInterface())) {
    return nullptr;
  }
  return tab;
}

void CloseTab(tabs::TabInterface* tab) {
  if (TabListInterface* list =
          TabListInterface::From(tab->GetBrowserWindowInterface())) {
    list->CloseTab(tab->GetHandle());
  }
}

Response TabStripBusy() {
  return Response::ServerError("a tab strip of the profile is being dragged");
}

// A new group of `tab` named `title` in `color`; its wire ID.
std::optional<int> StartGroup(tabs::TabInterface* tab,
                              const std::string& title,
                              tab_groups::TabGroupColorId color) {
  TabListInterface* list =
      TabListInterface::From(tab->GetBrowserWindowInterface());
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
    if (tab && !TabListInterface::CanEditTabList(
                   *tab->GetBrowserWindowInterface()->GetProfile())) {
      return false;
    }
  }
  return true;
}

}  // namespace

// static
bool PegboardHandler::IsAvailable(
    content::DevToolsAgentHostClientChannel* channel) {
  content::DevToolsAgentHostClient* client = channel->GetClient();
  return channel->GetAgentHost()->GetType() ==
             content::DevToolsAgentHost::kTypeBrowser &&
         client->IsTrusted() && client->AllowUnsafeOperations();
}

PegboardHandler::PegboardHandler(protocol::UberDispatcher* dispatcher)
    : frontend_(std::make_unique<protocol::Pegboard::Frontend>(
          dispatcher->channel())) {
  protocol::Pegboard::Dispatcher::wire(dispatcher, this);
}

PegboardHandler::~PegboardHandler() {
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
  tab_strip_tracker_ = std::make_unique<BrowserTabStripTracker>(this, nullptr);
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
  callback->sendSuccess(profile->UniqueId());
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
    TabListInterface::From(tab->GetBrowserWindowInterface())
        ->MoveTabToWindow(handle, window->GetSessionID(), list->GetTabCount());
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
    if (!tab || !tab->GetGroup()) {
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
  if (!IsAgentProfile(profile)) {
    return;
  }
  frontend_->TabActivated(profile->UniqueId(), TargetIdOf(contents),
                          base::Time::Now().InMillisecondsFSinceUnixEpoch());
}

}  // namespace pegboard
