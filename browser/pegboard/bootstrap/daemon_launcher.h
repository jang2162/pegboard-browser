// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PEGBOARD_BOOTSTRAP_DAEMON_LAUNCHER_H_
#define PEGBOARD_BOOTSTRAP_DAEMON_LAUNCHER_H_

#include <atomic>
#include <memory>

#include "base/containers/circular_deque.h"
#include "base/files/file_path.h"
#include "base/files/scoped_file.h"
#include "base/memory/scoped_refptr.h"
#include "base/memory/weak_ptr.h"
#include "base/process/process_handle.h"
#include "base/time/time.h"
#include "base/timer/timer.h"
#include "chrome/browser/chrome_browser_main_extra_parts.h"

namespace base {
template <typename T>
class RefCountedData;
}  // namespace base

namespace pegboard {

class DaemonNotice;

// Runs pegboardd as a child of the browser and hands it the browser's DevTools
// pipe (design 4.2): `Contents/Helpers/pegboard daemon --cdp-pipe`, reading
// fd 3 and writing fd 4, NUL-terminated JSON. The browser's ends go to
// content's pipe handler (content::DevToolsAgentHost::
// StartRemoteDebuggingPipeHandlerWithFds, devtools-pipe-fds.patch).
//
// - Nothing runs when the browser was started with --remote-debugging-pipe
//   (its parent has the pipe, F13) or --pegboard-no-daemon, or when the
//   app has no Contents/Helpers/pegboard. A process that only hands its
//   command line to the running browser never gets to PostBrowserStart.
// - When the daemon ends (its end of the pipe or the process), the pipe
//   handler stops and the daemon is started again, at most kMaxRestarts times
//   in kRestartWindow, and never after it exited with kExitLockHeld (another
//   daemon holds the lock). After that the browser goes on without one (F8)
//   and, when it gave up after kMaxRestarts (not for kExitLockHeld), shows
//   the person a DaemonNotice (//pegboard/ui) whose 다시 시작 forgets the
//   restarts so far and starts the daemon again; a daemon that starts closes
//   a notice still open.
// - When the browser quits, the pipe is closed (the daemon sees its end,
//   closes its sessions and exits); a daemon still there after kQuitWait gets
//   SIGTERM, and SIGKILL kKillWait later. A daemon that closed its end of the
//   pipe and stays gets the same.
// - The daemon is in its own process group and nothing of it is waited on by
//   the browser's own work: its death never takes the browser with it.
// - The descriptors are made close-on-exec right after the pipes are made, so
//   no other child of the browser holds the pipe. The daemon opens its own log
//   (<PEGBOARD_HOME>/daemon.log). It inherits the browser's environment, with
//   PEGBOARD_AGENT_BROWSER set to the bundle's Contents/Helpers/agent-browser
//   and, unless the browser itself has PEGBOARD_TEST_HOOKS, without any
//   PEGBOARD_TEST_* variable.
//
// Lives on the UI thread; MainParts (//pegboard/browser_parts, which
// pegboard-bootstrap.patch adds to Chrome's main parts) owns it and passes it
// the stages below.
class DaemonLauncher : public ChromeBrowserMainExtraParts {
 public:
  // The daemon's exit code when another daemon holds the lock
  // (crates/pegboard-core/src/lock.rs EXIT_LOCK_HELD).
  static constexpr int kExitLockHeld = 4;
  static constexpr size_t kMaxRestarts = 3;
  static constexpr base::TimeDelta kRestartWindow = base::Minutes(1);
  static constexpr base::TimeDelta kRestartDelay = base::Seconds(1);
  static constexpr base::TimeDelta kQuitWait = base::Seconds(5);
  // After SIGTERM, how long before SIGKILL (a restart must not wait forever).
  static constexpr base::TimeDelta kKillWait = base::Seconds(3);
  // The switch that keeps the browser from starting a daemon.
  static constexpr char kNoDaemonSwitch[] = "pegboard-no-daemon";

  DaemonLauncher();
  DaemonLauncher(const DaemonLauncher&) = delete;
  DaemonLauncher& operator=(const DaemonLauncher&) = delete;
  ~DaemonLauncher() override;

  // ChromeBrowserMainExtraParts:
  void PostBrowserStart() override;
  void PostMainMessageLoopRun() override;

 private:
  using ExitFlag = base::RefCountedData<std::atomic<bool>>;

  void Launch();
  // 다시 시작 on the notice: the restarts so far are forgotten.
  void RestartFromNotice();
  // The daemon's end of the pipe closed (content's handler saw it).
  void OnPipeClosed(int run);
  void OnExited(int run, int exit_code);
  // Stops the pipe handler and closes the browser's ends of the pipe.
  void EndPipe();
  // SIGTERM for the daemon of launch `run` if it is still there, then SIGKILL
  // after kKillWait.
  void TerminateIfRunning(int run);
  void KillIfRunning(int run);
  // Whether the daemon of launch `run` has not been seen gone.
  bool Running(int run) const;

  base::FilePath executable_;
  // Which launch the callbacks are about; stale ones are ignored.
  int run_ = 0;
  base::ProcessId pid_ = base::kNullProcessId;
  // Set by the thread that waits for the process, when it is gone (reaped).
  scoped_refptr<ExitFlag> exited_;
  // The browser's ends: it reads read_fd_ and writes write_fd_.
  base::ScopedFD read_fd_;
  base::ScopedFD write_fd_;
  bool pipe_running_ = false;
  bool quitting_ = false;
  // Times of the restarts in the last kRestartWindow.
  base::circular_deque<base::TimeTicks> restarts_;
  // SIGTERM for a daemon that closed its pipe and did not exit. Bound to its
  // launch: the pipe can be seen closed after the process was seen gone, and a
  // late timer must not reach the next daemon.
  base::OneShotTimer term_timer_;
  // The notice that the daemon will not be started again, while it is up.
  std::unique_ptr<DaemonNotice> notice_;
  base::WeakPtrFactory<DaemonLauncher> weak_factory_{this};
};

}  // namespace pegboard

#endif  // PEGBOARD_BOOTSTRAP_DAEMON_LAUNCHER_H_
