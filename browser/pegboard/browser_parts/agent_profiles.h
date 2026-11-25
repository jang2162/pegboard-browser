// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_BROWSER_PARTS_AGENT_PROFILES_H_
#define PEGBOARD_BROWSER_PARTS_AGENT_PROFILES_H_

#include <string>

#include "base/files/file_path.h"
#include "base/scoped_observation.h"
#include "chrome/browser/profiles/profile_attributes_storage.h"
#include "chrome/browser/profiles/profile_attributes_storage_observer.h"
#include "chrome/browser/ui/browser_window/public/browser_collection.h"
#include "chrome/browser/ui/browser_window/public/browser_collection_observer.h"
#include "third_party/skia/include/core/SkColor.h"

class Profile;

namespace pegboard {

// Keeps the agent profiles out of a person's way and makes their windows
// stand out (design 4.6, B12, B17). It goes by the agent profile rule alone
// (//pegboard/common/agent_profile.h): a person's profile is never marked,
// and a person's windows never get the title. It works whether or not a
// daemon runs, since it is about what the person sees.
//
// - Agent profiles are omitted from the profile list (the profile menu, the
//   profile picker, "open link as"): ProfileAttributesEntry::SetIsOmitted.
//   That is kept in memory only, so it is done at every start, before the
//   browser decides whether to show the picker, and for each agent profile
//   added later. (Upstream omits only ephemeral profiles; the flag's readers
//   do not depend on that, and agent profiles must not be ephemeral: those are
//   deleted.)
// - They do not bring up the profile picker at launch. Chrome shows it when
//   the profile list has more than one entry, omitted ones included
//   (ProfilePicker::GetStartupMode). So the default of the picker's
//   availability at startup (local state, a policy's pref) is "disabled"
//   while there is at most one person's profile; with two or more it is the
//   upstream default again. Only the default: a policy wins.
// - A launch does not open an agent profile from the last run: at start the
//   last used profile goes back to a person's when it names an agent one, and
//   agent profiles are taken out of the profiles a launch restores. (Only at
//   start: while the browser runs, the last used profile is what DevTools calls
//   the default browser context, which the daemon makes the agent profile to
//   set its download policy.)
// - Their frame wears one reserved color (a user color theme of the agent
//   profile, kAgentThemeColor) and their normal windows the title
//   kAgentWindowTitle (the window's user title, as "Name window").
//
// Lives on the UI thread, owned by MainParts.
class AgentProfiles : public ProfileAttributesStorage::Observer,
                      public BrowserCollectionObserver {
 public:
  // A dark amber, apart from the default grey and blue frames in light and
  // dark mode.
  static constexpr SkColor kAgentThemeColor = SkColorSetRGB(0xB4, 0x53, 0x09);
  static constexpr char kAgentWindowTitle[] = "Pegboard 에이전트";

  AgentProfiles();
  AgentProfiles(const AgentProfiles&) = delete;
  AgentProfiles& operator=(const AgentProfiles&) = delete;
  ~AgentProfiles() override;

  // Before the first profile is made (PreProfileInit).
  void Start();
  // Each profile as it is initialized (PostProfileInit).
  void OnProfileInit(Profile* profile);
  // Before the browser process tears down (PostMainMessageLoopRun).
  void Stop();

  // ProfileAttributesStorage::Observer:
  void OnProfileAdded(const base::FilePath& profile_path) override;
  void OnProfileWasRemoved(const base::FilePath& profile_path,
                           const std::u16string& profile_name) override;

  // BrowserCollectionObserver:
  void OnBrowserCreated(BrowserWindowInterface* browser) override;

 private:
  // Omits the entry of `path` when it is an agent profile's.
  void OmitIfAgent(const base::FilePath& path);
  void UpdatePickerDefault();
  // The last used profile, back to a person's when it names an agent one.
  void ForgetAgentAsLastUsed();

  base::FilePath user_data_dir_;
  base::ScopedObservation<ProfileAttributesStorage,
                          ProfileAttributesStorage::Observer>
      storage_observation_{this};
  base::ScopedObservation<BrowserCollection, BrowserCollectionObserver>
      browsers_observation_{this};
};

}  // namespace pegboard

#endif  // PEGBOARD_BROWSER_PARTS_AGENT_PROFILES_H_
