// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/browser_parts/profile_defaults.h"

#include "base/logging.h"
#include "base/values.h"
#include "chrome/browser/signin/account_consistency_mode_manager_factory.h"
#include "chrome/browser/signin/identity_manager_factory.h"
#include "components/keyed_service/core/keyed_service.h"
#include "components/password_manager/core/common/password_manager_pref_names.h"
#include "components/pref_registry/pref_registry_syncable.h"
#include "components/prefs/pref_store.h"
#include "components/signin/public/base/signin_pref_names.h"

namespace pegboard {

namespace {

// The preferences turned off (their policies, for reference: BrowserSignin=0,
// PasswordManagerEnabled, AutoSignInEnabled, PasswordManagerPasskeysEnabled).
constexpr const char* kOff[] = {
    // What the "Allow Chrome sign-in" setting writes; each start copies it
    // into signin.allowed. Both are off.
    prefs::kSigninAllowedOnNextStartup,
    prefs::kSigninAllowed,
    // Offer to save passwords.
    password_manager::prefs::kCredentialsEnableService,
    // Sign in automatically (Credential Management API).
    password_manager::prefs::kCredentialsEnableAutosignin,
    // Offer to save passkeys in the password manager.
    password_manager::prefs::kCredentialsEnablePasskeys,
};

}  // namespace

// static
void ProfileDefaults::Register() {
  static base::NoDestructor<ProfileDefaults> instance;
}

ProfileDefaults::ProfileDefaults()
    : ProfileKeyedServiceFactory("PegboardProfileDefaults") {
  // After the factories that register the preferences above (the dependency
  // order is also the order preferences are registered in).
  DependsOn(AccountConsistencyModeManagerFactory::GetInstance());
  DependsOn(IdentityManagerFactory::GetInstance());
}

ProfileDefaults::~ProfileDefaults() = default;

std::unique_ptr<KeyedService>
ProfileDefaults::BuildServiceInstanceForBrowserContext(
    content::BrowserContext* context) const {
  return nullptr;
}

void ProfileDefaults::RegisterProfilePrefs(
    user_prefs::PrefRegistrySyncable* registry) {
  for (const char* name : kOff) {
    const base::Value* value = nullptr;
    if (!registry->defaults()->GetValue(name, &value) || !value->is_bool()) {
      LOG(ERROR) << "pegboard: no boolean preference " << name;
      continue;
    }
    registry->SetDefaultPrefValue(name, base::Value(false));
  }
}

}  // namespace pegboard
