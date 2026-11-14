export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
echo "CLIPBEFORE :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CLIP :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1 2>&1 | f)"
echo "CLIPMID :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
echo "INFO :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1 --nocapture sandboxed_srcdoc agent_browser_recovers 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running)|pegboard guard:' | grep -i -E 'frame|recover|next command|reload|panicked' | cut -c1-200 | head -8 | tr '\n' ' ')"
for t in e2e_policy e2e_runner e2e_session e2e_proxy; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
for t in e2e_policy e2e_isolation e2e_cli e2e_mcp; do echo "CLI $t :: $(cargo test -p pegboard-cli --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
oc() { cargo test -p pegboard-core --test e2e_clipboard -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
C=crates/pegboard-core/src; G=$C/guard.rs; DL=$C/downloads.rs; A=$C/argv.rs; DA=$C/daemon.rs
rep $G 'let shield = run_immediately.map(' 'let shield = run_immediately.filter(|_| false).map(' && echo "M1 :: $(oc agent_cannot_write_the_clipboard_by_default) $(oc new_tabs_popups_and_frames_are_covered)"; git checkout -q -- .
rep $DL 'for name in ["clipboard-read", "clipboard-write"] {' 'for name in [] as [&str; 0] {' && echo "M2 :: $(oc agent_cannot_read_the_clipboard_by_default)"; git checkout -q -- .
rep $A '.is_some_and(|k| is_clipboard_key(k)) =>' '.is_some_and(|k| is_clipboard_key(k) && false) =>' && echo "M3 :: $(oc agent_cannot_write_the_clipboard_by_default)"; git checkout -q -- .
rep $G 'params["runImmediately"] = json!(true);' '' && echo "M5 :: $(oc a_kept_tab_gets_the_shield_into_its_running_document_after_a_reconnect)"; git checkout -q -- .
rep $DA '    if let Err(e) = sessions.require_clipboard(id) {
        return refuse(log, entry, argv, e, started);
    }' '' && echo "M6 :: $(oc clipboard_denial_is_set_again_after_its_connection_drops)"; git checkout -q -- .
rep $DL '"Browser.resetPermissions",' '"Browser.getVersion",' && echo "M7 :: $(oc clipboard_setting_of_one_session_does_not_leak_to_another)"; git checkout -q -- .
rep $C/session.rs 'if ids.is_empty() {' 'if true {' && echo "M8 :: $(oc after_a_guard_reconnect_agent_connections_are_dropped_and_tabs_reloaded)"; git checkout -q -- .
rep $G '"Page.reload"' '"Page.getFrameTree"' && echo "M9 :: $(oc after_a_guard_reconnect_agent_connections_are_dropped_and_tabs_reloaded)"; git checkout -q -- .
rep $G '"location.reload()"' '"1"' && echo "M10 :: $(oc sandboxed_srcdoc_frame_listening_first)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo "CLIPAFTER :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
echo T7D_DONE
