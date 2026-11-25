// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/browser_parts/agent_profiles.h"

#include <utility>

#include "base/functional/bind.h"
#include "base/values.h"
#include "chrome/browser/browser_process.h"
#include "chrome/browser/profiles/profile.h"
#include "chrome/browser/profiles/profile_attributes_entry.h"
#include "chrome/browser/profiles/profile_manager.h"
#include "chrome/browser/themes/theme_service.h"
#include "chrome/browser/themes/theme_service_factory.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface.h"
#include "chrome/browser/ui/browser_window/public/browser_window_interface_iterator.h"
#include "chrome/browser/ui/browser_window/public/global_browser_collection.h"
#include "chrome/browser/ui/profiles/profile_picker.h"
#include "chrome/browser/ui/window_metadata/window_metadata_controller.h"
#include "chrome/common/chrome_constants.h"
#include "chrome/common/pref_names.h"
#include "components/prefs/pref_service.h"
#include "components/prefs/scoped_user_pref_update.h"
#include "pegboard/common/agent_profile.h"
#include "pegboard/ui/test_hooks.h"
#include "ui/base/mojom/themes.mojom.h"

namespace pegboard {

namespace {

constexpr ui::mojom::BrowserColorVariant kAgentColorVariant =
    ui::mojom::BrowserColorVariant::kVibrant;

ProfileAttributesStorage* Storage() {
  ProfileManager* manager = g_browser_process->profile_manager();
  return manager ? &manager->GetProfileAttributesStorage() : nullptr;
}

}  // namespace

AgentProfiles::AgentProfiles() = default;

AgentProfiles::~AgentProfiles() = default;

void AgentProfiles::Start() {
  ProfileManager* manager = g_browser_process->profile_manager();
  PrefService* local_state = g_browser_process->local_state();
  if (!manager || !local_state) {
    return;
  }
  user_data_dir_ = manager->user_data_dir();
  ProfileAttributesStorage& storage = manager->GetProfileAttributesStorage();
  for (ProfileAttributesEntry* entry : storage.GetAllProfilesAttributes()) {
    OmitIfAgent(entry->GetPath());
  }
  storage_observation_.Observe(&storage);
  UpdatePickerDefault();

  // The profiles this launch restores (with "continue where you left
  // off", or after a crash): no agent profile among them. The daemon closes
  // agent windows anyway (F11); they would only flash.
  if (local_state->HasPrefPath(prefs::kProfilesLastActive)) {
    ScopedListPrefUpdate update(local_state, prefs::kProfilesLastActive);
    update->EraseIf([](const base::Value& name) {
      return name.is_string() && IsAgentDirectoryName(name.GetString());
    });
  }
  ForgetAgentAsLastUsed();

  browsers_observation_.Observe(GlobalBrowserCollection::GetInstance());
#if BUILDFLAG(IS_MAC)
  main_window_watch_ = std::make_unique<app_last_profile::MainWindowWatch>(
      base::BindRepeating(&AgentProfiles::KeepAppLastProfileHuman,
                          weak_factory_.GetWeakPtr()));
#endif
}

void AgentProfiles::Stop() {
  weak_factory_.InvalidateWeakPtrs();
#if BUILDFLAG(IS_MAC)
  main_window_watch_.reset();
#endif
  browsers_observation_.Reset();
  storage_observation_.Reset();
}

void AgentProfiles::OmitIfAgent(const base::FilePath& path) {
  if (path.DirName() != user_data_dir_ ||
      !IsAgentDirectoryName(path.BaseName().AsUTF8Unsafe())) {
    return;
  }
  ProfileAttributesStorage* storage = Storage();
  ProfileAttributesEntry* entry =
      storage ? storage->GetProfileAttributesWithPath(path) : nullptr;
  if (entry && !entry->IsOmitted()) {
    entry->SetIsOmitted(true);
  }
}

void AgentProfiles::UpdatePickerDefault() {
  ProfileAttributesStorage* storage = Storage();
  PrefService* local_state = g_browser_process->local_state();
  if (!storage || !local_state) {
    return;
  }
  size_t people = 0;
  for (ProfileAttributesEntry* entry : storage->GetAllProfilesAttributes()) {
    if (!IsAgentDirectoryName(entry->GetPath().BaseName().AsUTF8Unsafe())) {
      ++people;
    }
  }
  // Upstream's rule with the agent profiles left out: no picker at launch
  // with at most one profile.
  const ProfilePicker::AvailabilityOnStartup availability =
      people > 1 ? ProfilePicker::AvailabilityOnStartup::kEnabled
                 : ProfilePicker::AvailabilityOnStartup::kDisabled;
  local_state->SetDefaultPrefValue(
      prefs::kBrowserProfilePickerAvailabilityOnStartup,
      base::Value(static_cast<int>(availability)));
}

std::string AgentProfiles::LastPersonsDirectory() const {
  ProfileAttributesStorage* storage = Storage();
  std::string human;
  if (!storage) {
    return human;
  }
  base::Time latest;
  for (ProfileAttributesEntry* entry : storage->GetAllProfilesAttributes()) {
    const std::string dir = entry->GetPath().BaseName().AsUTF8Unsafe();
    if (entry->GetPath().DirName() == user_data_dir_ &&
        !IsAgentDirectoryName(dir) &&
        (human.empty() || entry->GetActiveTime() > latest)) {
      human = dir;
      latest = entry->GetActiveTime();
    }
  }
  return human;
}

bool AgentProfiles::IsAgent(const Profile* profile) const {
  if (!profile) {
    return false;
  }
  const Profile* original = profile->GetOriginalProfile();
  return !original->IsSystemProfile() && !original->IsGuestSession() &&
         original->GetPath().DirName() == user_data_dir_ &&
         IsAgentDirectoryName(original->GetBaseName().AsUTF8Unsafe());
}

void AgentProfiles::ForgetAgentAsLastUsed() {
  PrefService* local_state = g_browser_process->local_state();
  if (!local_state ||
      !IsAgentDirectoryName(local_state->GetString(prefs::kProfileLastUsed))) {
    return;
  }
  // The person's profile active most recently, else the default one.
  const std::string human = LastPersonsDirectory();
  if (human.empty()) {
    local_state->ClearPref(prefs::kProfileLastUsed);
  } else {
    local_state->SetString(prefs::kProfileLastUsed, human);
  }
}

void AgentProfiles::OnBrowserClosed(BrowserWindowInterface* browser) {
  // The last agent window gone, no daemon needs an agent profile as the
  // default context: the last used profile goes back to a person's (a Dock
  // click with no window, or the next launch, opens that).
  for (BrowserWindowInterface* other : GetAllBrowserWindowInterfaces()) {
    if (other != browser && IsAgent(other->GetProfile())) {
      return;
    }
  }
  ForgetAgentAsLastUsed();
}

void AgentProfiles::KeepAppLastProfileHuman() {
#if BUILDFLAG(IS_MAC)
  if (!IsAgent(app_last_profile::Get())) {
    return;
  }
  // The profile of the person's window active most recently.
  Profile* human = nullptr;
  ForEachCurrentBrowserWindowInterfaceOrderedByActivation(
      [this, &human](BrowserWindowInterface* window) {
        Profile* profile = window->GetProfile();
        if (window->GetType() == BrowserWindowInterface::TYPE_NORMAL &&
            profile && !IsAgent(profile)) {
          human = profile;
          return false;
        }
        return true;
      });
  ProfileManager* manager = g_browser_process->profile_manager();
  std::string dir = LastPersonsDirectory();
  if (dir.empty()) {
    dir = chrome::kInitialProfile;
  }
  const base::FilePath path = user_data_dir_.AppendASCII(dir);
  if (!human && manager) {
    human = manager->GetProfileByPath(path);
  }
  if (human) {
    app_last_profile::Set(human);
    return;
  }
  if (!manager) {
    return;
  }
  // No person's profile is loaded: load the last one used.
  manager->CreateProfileAsync(
      path, base::BindOnce(
                [](base::WeakPtr<AgentProfiles> self, Profile* profile) {
                  if (self && profile && !self->IsAgent(profile) &&
                      self->IsAgent(app_last_profile::Get())) {
                    app_last_profile::Set(profile);
                  }
                },
                weak_factory_.GetWeakPtr()));
#endif
}

void AgentProfiles::OnProfileAdded(const base::FilePath& profile_path) {
  OmitIfAgent(profile_path);
  UpdatePickerDefault();
}

void AgentProfiles::OnProfileWasRemoved(const base::FilePath& profile_path,
                                        const std::u16string& profile_name) {
  UpdatePickerDefault();
}

void AgentProfiles::OnProfileInit(Profile* profile) {
  if (!profile || profile->IsOffTheRecord() || profile->IsSystemProfile() ||
      profile->IsGuestSession() ||
      profile->GetPath().DirName() != user_data_dir_ ||
      !IsAgentDirectoryName(profile->GetBaseName().AsUTF8Unsafe())) {
    return;
  }
  ThemeService* theme = ThemeServiceFactory::GetForProfile(profile);
  if (!theme) {
    return;
  }
  if (theme->GetUserColor() != kAgentThemeColor ||
      theme->GetBrowserColorVariant() != kAgentColorVariant) {
    theme->SetUserColorAndBrowserColorVariant(kAgentThemeColor,
                                              kAgentColorVariant);
  }
}

void AgentProfiles::OnBrowserCreated(BrowserWindowInterface* browser) {
  if (!browser || browser->GetType() != BrowserWindowInterface::TYPE_NORMAL) {
    return;
  }
  Profile* profile = browser->GetProfile();
  const std::string dir =
      profile ? profile->GetBaseName().AsUTF8Unsafe() : std::string();
  const bool agent = profile && !profile->IsOffTheRecord() &&
                     profile->GetPath().DirName() == user_data_dir_ &&
                     IsAgentDirectoryName(dir);
  WindowMetadataController* metadata = WindowMetadataController::From(browser);
  if (agent && metadata) {
    metadata->SetWindowUserTitle(kAgentWindowTitle);
  }
  if (test_hooks::Enabled()) {
    base::DictValue entry;
    entry.Set("kind", "window");
    entry.Set("profile", dir);
    entry.Set("userTitle", metadata ? metadata->user_title() : std::string());
    test_hooks::LogDialog(entry);
  }
}

}  // namespace pegboard
