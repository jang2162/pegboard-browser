export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
pc() { cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 "$@" 2>&1 | f; }
pi() { cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 "$@" 2>&1 | f; }
pcore() { cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 "$@" 2>&1 | f; }
echo "CORE_POLICY :: $(pcore)"
echo "CLI_POLICY :: $(pc)"
echo "CLI_ISOLATION :: $(pi)"
for t in e2e_proxy e2e_session e2e_runner e2e_actionlog e2e_bridge e2e_browser e2e_isolated_world; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
echo "CLI e2e_cli :: $(cargo test -p pegboard-cli --test e2e_cli -- --ignored --test-threads=1 2>&1 | f)"
echo "DLOG :: $(grep -h 'pegboard: ' $(ls -t /var/folders/*/*/T/*/daemon.log 2>/dev/null | head -3) 2>/dev/null | grep -i -E 'download|policy|cancel' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-160 | head -8 | tr '\n' ' ')"
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
F=crates/pegboard-core/src/downloads.rs; D=crates/pegboard-core/src/daemon.rs
rep $F 'session = self.session_of_frame(frame).await;' 'session = None;' && echo "M1 :: $(pc download_lands_in_the_session_files_dir_only)"; git checkout -q -- .
rep $F '(Some(_), Some(v)) if !v.allows_downloads =>' '(Some(_), Some(v)) if false && !v.allows_downloads =>' && echo "M2 :: $(pc deny_download_writes_no_file_anywhere)"; git checkout -q -- .
rep $F '            std::env::var("PEGBOARD_TEST_DOWNLOAD_LIMITS")
                .ok()
                .as_deref(),' '            None,' && echo "M3 :: $(pc oversized_download_is_dropped_and_reported)"; git checkout -q -- .
rep $D '    if let Err(e) = sessions.require_downloads(id) {
        return refuse(log, entry, argv, e, started);
    }' '' && echo "M4 :: $(pcore download_policy_is_reapplied_after_its_connection_drops)"; git checkout -q -- .
rep $F '        if start {' '        if false && start {' && echo "M5 :: $(pcore download_policy_is_reapplied_after_its_connection_drops)"; git checkout -q -- .
rep $F '                    let _ = std::fs::remove_file(path);' '                    let _ = path;' && echo "M6 :: $(pi unattributed_download_is_deleted)"; git checkout -q -- .
rep $F '            .reports
            .remove(session)' '            .reports
            .get(session).cloned()' && echo "M7 :: $(pc download_lands_in_the_session_files_dir_only)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DLLEFT :: $(ls ~/Downloads | grep -c -i peg)"
echo T5A_DONE
