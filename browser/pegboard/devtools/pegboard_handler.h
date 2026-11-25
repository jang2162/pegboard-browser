// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_DEVTOOLS_PEGBOARD_HANDLER_H_
#define PEGBOARD_DEVTOOLS_PEGBOARD_HANDLER_H_

#include <map>
#include <memory>
#include <optional>
#include <string>

#include "base/memory/weak_ptr.h"
#include "base/scoped_multi_source_observation.h"
#include "base/scoped_observation.h"
#include "base/timer/timer.h"
#include "chrome/browser/devtools/protocol/pegboard.h"
#include "chrome/browser/profiles/profile_manager.h"
#include "chrome/browser/profiles/profile_manager_observer.h"
#include "chrome/browser/profiles/profile_observer.h"
#include "chrome/browser/ui/browser_tab_strip_tracker.h"
#include "chrome/browser/ui/browser_tab_strip_tracker_delegate.h"
#include "chrome/browser/ui/tabs/tab_strip_model_observer.h"
#include "components/saved_tab_groups/public/tab_group_sync_service.h"
#include "pegboard/ui/confirm_dialog.h"

class Profile;

namespace content {
class DevToolsAgentHostClientChannel;
}  // namespace content

namespace pegboard {

class Confirmations;

// Calls back once a profile's saved-tab-group service has started (true), or
// went or took too long (false); in a later task, never from its own methods.
class TabGroupSyncWaiter : public tab_groups::TabGroupSyncService::Observer {
 public:
  TabGroupSyncWaiter(tab_groups::TabGroupSyncService* service,
                     base::OnceCallback<void(bool)> done);
  TabGroupSyncWaiter(const TabGroupSyncWaiter&) = delete;
  TabGroupSyncWaiter& operator=(const TabGroupSyncWaiter&) = delete;
  ~TabGroupSyncWaiter() override;

  // tab_groups::TabGroupSyncService::Observer:
  void OnInitialized() override;
  void OnWillBeDestroyed() override;

 private:
  void Finish(bool started);

  base::OnceCallback<void(bool)> done_;
  base::OneShotTimer timer_;
  base::ScopedObservation<tab_groups::TabGroupSyncService,
                          tab_groups::TabGroupSyncService::Observer>
      observation_{this};
};

// The Pegboard domain on one browser session of the DevTools pipe client
// (pegboard_protocol.pdl). Exactly one session at a time owns the domain: the
// first to call Pegboard.claim, until it goes. On every other session each
// method answers -32601 as an unknown one. (The generated dispatcher checks
// the parameters first, so a malformed call gets -32602 there instead; that
// says nothing beyond that the domain is defined.) Only the owner observes the
// browser, and only agent profiles' normal windows, so the domain's events go
// to the owner alone.
class PegboardHandler : public protocol::Pegboard::Backend,
                        public ProfileManagerObserver,
                        public ProfileObserver,
                        public TabStripModelObserver,
                        public BrowserTabStripTrackerDelegate {
 public:
  // Whether `channel` may have the domain at all: a browser target session of
  // the DevTools pipe client, its own or one opened on it. That is, a trusted
  // client that may do unsafe operations (not chrome.debugger, the DevTools
  // front end or the remote debugging port) and that is the pipe or a session
  // of content's TargetHandler (not a Target.exposeDevToolsProtocol binding).
  static bool IsAvailable(content::DevToolsAgentHostClientChannel* channel);

  explicit PegboardHandler(protocol::UberDispatcher* dispatcher);
  PegboardHandler(const PegboardHandler&) = delete;
  PegboardHandler& operator=(const PegboardHandler&) = delete;
  ~PegboardHandler() override;

  // protocol::Pegboard::Backend:
  protocol::Response Claim() override;
  protocol::Response GetProfiles(
      std::unique_ptr<protocol::Array<protocol::Pegboard::ProfileInfo>>*
          out_profiles) override;
  void OpenProfileWindow(
      const std::string& profile_directory,
      std::unique_ptr<OpenProfileWindowCallback> callback) override;
  protocol::Response CreateGroup(const std::string& browser_context_id,
                                 const std::string& title,
                                 const std::string& color,
                                 int* out_group_id,
                                 std::string* out_target_id) override;
  protocol::Response CreateTab(const std::string& browser_context_id,
                               int group_id,
                               const std::string& url,
                               const std::string& title,
                               const std::string& color,
                               std::string* out_target_id,
                               std::optional<int>* out_new_group_id) override;
  protocol::Response AddToGroup(int group_id,
                                const std::string& target_id) override;
  protocol::Response GetGroupMembers(
      int group_id,
      std::unique_ptr<protocol::Array<std::string>>* out_target_ids) override;
  protocol::Response CloseTargets(
      std::unique_ptr<protocol::Array<std::string>> target_ids) override;
  protocol::Response UngroupTargets(
      std::unique_ptr<protocol::Array<std::string>> target_ids) override;
  protocol::Response Ungroup(int group_id) override;
  protocol::Response RequestConfirmation(
      const std::string& confirm_id,
      const std::string& browser_context_id,
      std::unique_ptr<protocol::Array<std::string>> target_ids,
      const std::string& session_id,
      const std::string& session_name,
      const std::string& profile_directory,
      const std::string& action_class,
      std::unique_ptr<protocol::Array<protocol::Pegboard::ConfirmationDetail>>
          details,
      bool offer_allow_for_session) override;
  protocol::Response CancelConfirmation(const std::string& confirm_id) override;

  // ProfileManagerObserver:
  void OnProfileAdded(Profile* profile) override;
  void OnProfileManagerDestroying() override;

  // ProfileObserver:
  void OnProfileWillBeDestroyed(Profile* profile) override;

  // BrowserTabStripTrackerDelegate:
  bool ShouldTrackBrowser(BrowserWindowInterface* browser) override;

  // TabStripModelObserver:
  void OnTabStripModelChanged(
      TabStripModel* tab_strip_model,
      const TabStripModelChange& change,
      const TabStripSelectionChange& selection) override;

 private:
  bool IsOwner() const;
  // The answer of a non-owner: as if the method did not exist.
  static protocol::Response NotFound(const char* method);
  // Starts observing profiles and tab strips (on claim).
  void StartObserving();
  void ObserveProfile(Profile* profile);
  void OnProfileReady(std::string profile_directory,
                      std::unique_ptr<OpenProfileWindowCallback> callback,
                      Profile* profile);
  void OnConfirmationAnswered(const std::string& confirm_id,
                              ConfirmAnswer answer);
  void OnTabGroupSyncStarted(
      int waiter,
      std::string browser_context_id,
      std::string profile_directory,
      std::unique_ptr<OpenProfileWindowCallback> callback,
      bool started);

  std::unique_ptr<protocol::Pegboard::Frontend> frontend_;
  base::ScopedObservation<ProfileManager, ProfileManagerObserver>
      profile_manager_observation_{this};
  base::ScopedMultiSourceObservation<Profile, ProfileObserver>
      profile_observations_{this};
  std::unique_ptr<BrowserTabStripTracker> tab_strip_tracker_;
  // openProfileWindow calls waiting for a profile's tab groups to start.
  std::map<int, std::unique_ptr<TabGroupSyncWaiter>> waiters_;
  int next_waiter_id_ = 1;
  // The owner's confirmation dialogs (requestConfirmation). They go with it,
  // before the frontend.
  std::unique_ptr<Confirmations> confirmations_;
  base::WeakPtrFactory<PegboardHandler> weak_factory_{this};
};

}  // namespace pegboard

#endif  // PEGBOARD_DEVTOOLS_PEGBOARD_HANDLER_H_
