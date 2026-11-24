// Copyright 2026 The Pegboard Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pegboard/bootstrap/daemon_launcher.h"

#include <fcntl.h>
#include <signal.h>
#include <unistd.h>

#include <utility>

#include "base/apple/bundle_locations.h"
#include "base/command_line.h"
#include "base/files/file_util.h"
#include "base/functional/bind.h"
#include "base/logging.h"
#include "base/memory/ref_counted.h"
#include "base/posix/eintr_wrapper.h"
#include "base/process/launch.h"
#include "base/process/process.h"
#include "base/task/sequenced_task_runner.h"
#include "base/task/thread_pool.h"
#include "base/threading/platform_thread.h"
#include "content/public/browser/devtools_agent_host.h"
#include "content/public/common/content_switches.h"

namespace pegboard {

namespace {

// The daemon's descriptors: it reads 3 and writes 4 (content's
// --remote-debugging-pipe numbers, which pegboardd's --cdp-pipe takes too).
constexpr int kDaemonReadFd = content::DevToolsAgentHost::kReadFD;
constexpr int kDaemonWriteFd = content::DevToolsAgentHost::kWriteFD;

// How long a retired descriptor stays open (as /dev/null) before it is closed.
// content's pipe threads may still touch the number for a moment after the
// handler is stopped; by then they are long gone, and the number cannot have
// been handed to anything else in between.
constexpr base::TimeDelta kRetiredFdLinger = base::Seconds(10);

// A close-on-exec descriptor at 10 or above: posix_spawn applies the
// remapping to 3 and 4 in order, so a source already at 3 or 4 could be
// overwritten by the other before it is copied.
base::ScopedFD HighCloexec(base::ScopedFD fd) {
  if (!fd.is_valid()) {
    return fd;
  }
  return base::ScopedFD(HANDLE_EINTR(fcntl(fd.get(), F_DUPFD_CLOEXEC, 10)));
}

// A pipe whose both ends are close-on-exec, so no child of the browser but
// the daemon (through the remapping) holds an end.
bool CloexecPipe(base::ScopedFD* read_end, base::ScopedFD* write_end) {
  base::ScopedFD r, w;
  if (!base::CreatePipe(&r, &w)) {
    return false;
  }
  *read_end = HighCloexec(std::move(r));
  *write_end = HighCloexec(std::move(w));
  return read_end->is_valid() && write_end->is_valid();
}

// Closes the pipe end at `fd` now (the other side sees its end) while keeping
// the number taken by /dev/null for a while (kRetiredFdLinger).
void Retire(base::ScopedFD fd) {
  if (!fd.is_valid()) {
    return;
  }
  base::ScopedFD null(HANDLE_EINTR(open("/dev/null", O_RDWR | O_CLOEXEC)));
  if (!null.is_valid() || HANDLE_EINTR(dup2(null.get(), fd.get())) < 0) {
    // Nothing to put in its place: close it at once.
    return;
  }
  base::SetCloseOnExec(fd.get());
  base::ThreadPool::PostDelayedTask(
      FROM_HERE, {base::MayBlock()},
      base::BindOnce([](base::ScopedFD) {}, std::move(fd)), kRetiredFdLinger);
}

}  // namespace

DaemonLauncher::DaemonLauncher() = default;

DaemonLauncher::~DaemonLauncher() = default;

void DaemonLauncher::PostBrowserStart() {
  const base::CommandLine& command_line =
      *base::CommandLine::ForCurrentProcess();
  if (command_line.HasSwitch(switches::kRemoteDebuggingPipe)) {
    return;  // The parent has the pipe (F13).
  }
  if (command_line.HasSwitch(kNoDaemonSwitch)) {
    return;
  }
  executable_ = base::apple::OuterBundlePath()
                    .Append("Contents")
                    .Append("Helpers")
                    .Append("pegboard");
  if (!base::PathExists(executable_)) {
    LOG(WARNING) << "pegboard: no " << executable_ << "; no daemon";
    return;
  }
  Launch();
}

void DaemonLauncher::Launch() {
  if (quitting_) {
    return;
  }
  const int run = ++run_;
  term_timer_.Stop();
  base::ScopedFD daemon_read, browser_write;  // browser -> daemon
  base::ScopedFD browser_read, daemon_write;  // daemon -> browser
  if (!CloexecPipe(&daemon_read, &browser_write) ||
      !CloexecPipe(&browser_read, &daemon_write)) {
    PLOG(ERROR) << "pegboard: making the DevTools pipe";
    OnExited(run, -1);
    return;
  }

  base::CommandLine daemon(executable_);
  daemon.AppendArg("daemon");
  daemon.AppendArg("--cdp-pipe");
  base::LaunchOptions options;
  options.fds_to_remap.emplace_back(daemon_read.get(), kDaemonReadFd);
  options.fds_to_remap.emplace_back(daemon_write.get(), kDaemonWriteFd);
  // Signals for the browser's process group (a Ctrl-C in the terminal that
  // started it) are not the daemon's.
  options.new_process_group = true;
  base::Process process = base::LaunchProcess(daemon, options);
  // Only the daemon holds its ends, so the browser sees the pipe end when the
  // daemon goes.
  daemon_read.reset();
  daemon_write.reset();
  if (!process.IsValid()) {
    LOG(ERROR) << "pegboard: could not start " << executable_;
    OnExited(run, -1);
    return;
  }
  pid_ = process.Pid();
  exited_ = base::MakeRefCounted<ExitFlag>();
  LOG(WARNING) << "pegboard: started daemon " << pid_;

  read_fd_ = std::move(browser_read);
  write_fd_ = std::move(browser_write);
  pipe_running_ = true;
  // content runs this on the UI thread from inside its handler; the handler
  // is stopped from a task of its own.
  content::DevToolsAgentHost::StartRemoteDebuggingPipeHandlerWithFds(
      read_fd_.get(), write_fd_.get(),
      base::BindOnce(
          [](base::WeakPtr<DaemonLauncher> launcher, int run) {
            base::SequencedTaskRunner::GetCurrentDefault()->PostTask(
                FROM_HERE, base::BindOnce(&DaemonLauncher::OnPipeClosed,
                                          launcher, run));
          },
          weak_factory_.GetWeakPtr(), run));

  // Waits for the process off the UI thread; the answer comes back here.
  base::ThreadPool::PostTaskAndReplyWithResult(
      FROM_HERE,
      {base::MayBlock(), base::WithBaseSyncPrimitives(),
       base::TaskShutdownBehavior::CONTINUE_ON_SHUTDOWN},
      base::BindOnce(
          [](base::Process process, scoped_refptr<ExitFlag> exited) {
            int exit_code = -1;
            if (!process.WaitForExit(&exit_code)) {
              exit_code = -1;
            }
            exited->data.store(true);
            return exit_code;
          },
          std::move(process), exited_),
      base::BindOnce(&DaemonLauncher::OnExited, weak_factory_.GetWeakPtr(),
                     run));
}

void DaemonLauncher::OnPipeClosed(int run) {
  // Also stale once the process is gone (its exit can be seen first).
  if (run != run_ || pid_ == base::kNullProcessId) {
    return;
  }
  EndPipe();
  // It normally exits right after; one that stays is told to.
  term_timer_.Start(FROM_HERE, kQuitWait,
                    base::BindOnce(&DaemonLauncher::TerminateIfRunning,
                                   base::Unretained(this), run));
}

void DaemonLauncher::OnExited(int run, int exit_code) {
  if (run != run_) {
    return;
  }
  EndPipe();
  term_timer_.Stop();
  pid_ = base::kNullProcessId;
  exited_ = nullptr;
  if (quitting_) {
    return;
  }
  LOG(WARNING) << "pegboard: daemon exited (" << exit_code << ")";
  if (exit_code == kExitLockHeld) {
    // Another daemon serves this pegboard home; starting again cannot help.
    LOG(WARNING) << "pegboard: another daemon holds the lock; not restarting";
    return;
  }
  const base::TimeTicks now = base::TimeTicks::Now();
  while (!restarts_.empty() && now - restarts_.front() >= kRestartWindow) {
    restarts_.pop_front();
  }
  if (restarts_.size() >= kMaxRestarts) {
    LOG(ERROR) << "pegboard: the daemon ended " << kMaxRestarts + 1
               << " times within " << kRestartWindow
               << "; not restarting it";
    return;
  }
  restarts_.push_back(now);
  base::SequencedTaskRunner::GetCurrentDefault()->PostDelayedTask(
      FROM_HERE,
      base::BindOnce(&DaemonLauncher::Launch, weak_factory_.GetWeakPtr()),
      kRestartDelay);
}

void DaemonLauncher::EndPipe() {
  if (!pipe_running_) {
    return;
  }
  pipe_running_ = false;
  content::DevToolsAgentHost::StopRemoteDebuggingPipeHandler();
  Retire(std::move(write_fd_));
  Retire(std::move(read_fd_));
}

void DaemonLauncher::TerminateIfRunning(int run) {
  if (run == run_ && pid_ != base::kNullProcessId && exited_ &&
      !exited_->data.load()) {
    LOG(WARNING) << "pegboard: daemon " << pid_ << " is still there; SIGTERM";
    kill(pid_, SIGTERM);
  }
}

void DaemonLauncher::PostMainMessageLoopRun() {
  quitting_ = true;
  weak_factory_.InvalidateWeakPtrs();
  term_timer_.Stop();
  if (pid_ == base::kNullProcessId) {
    return;
  }
  // The daemon sees the end of the pipe, closes its sessions and exits.
  EndPipe();
  // Polled rather than waited on: the main loop is over, and this must not
  // depend on the waiting thread getting to run.
  const base::TimeTicks deadline = base::TimeTicks::Now() + kQuitWait;
  while (!exited_->data.load() && base::TimeTicks::Now() < deadline) {
    base::PlatformThread::Sleep(base::Milliseconds(50));
  }
  TerminateIfRunning(run_);
}

}  // namespace pegboard
