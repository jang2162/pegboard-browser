// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_BROWSER_PARTS_PROFILE_DEFAULTS_H_
#define PEGBOARD_BROWSER_PARTS_PROFILE_DEFAULTS_H_

#include <memory>

#include "base/no_destructor.h"
#include "chrome/browser/profiles/profile_keyed_service_factory.h"

namespace pegboard {

// Pegboard's defaults for every profile's preferences (design 4.6, B13):
// sign-in to the browser, password saving, automatic sign-in and passkey
// saving in the built-in password manager are off. Only the defaults: a value
// the person chose (in settings, or kept from before) wins, and no policy is
// involved (no "managed by your organization").
//
// They are set where each profile's preferences are registered, before the
// profile's services are made and read them: sign-in is decided once, when
// AccountConsistencyModeManager is made with the profile (from
// signin.allowed_on_next_startup, into signin.allowed). So this is a keyed
// service factory with no service, which only changes defaults in
// RegisterProfilePrefs, after the factories that register those preferences
// (DependsOn orders that). Register() must run before the first profile is
// made (MainParts::PreProfileInit).
class ProfileDefaults : public ProfileKeyedServiceFactory {
 public:
  // Signs the factory up for every profile made from now on.
  static void Register();

  ProfileDefaults(const ProfileDefaults&) = delete;
  ProfileDefaults& operator=(const ProfileDefaults&) = delete;

 private:
  friend base::NoDestructor<ProfileDefaults>;

  ProfileDefaults();
  ~ProfileDefaults() override;

  // BrowserContextKeyedServiceFactory:
  std::unique_ptr<KeyedService> BuildServiceInstanceForBrowserContext(
      content::BrowserContext* context) const override;
  void RegisterProfilePrefs(
      user_prefs::PrefRegistrySyncable* registry) override;
};

}  // namespace pegboard

#endif  // PEGBOARD_BROWSER_PARTS_PROFILE_DEFAULTS_H_
