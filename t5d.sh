export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CORE_POLICY :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "NONDEFAULT :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --exact a_limit_cancel_works_when_the_agent_profile_is_not_the_default --nocapture 2>&1 | grep -i -E 'cancels|panicked|test result' | cut -c1-200 | tr '\n' ' ')"
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "GIVEUP :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact a_profile_whose_download_policy_stays_lost_loses_its_sessions --nocapture 2>&1 | grep -E 'pegboard|panicked|test result' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -10 | tr '\n' ' ')"
echo "CLI_ISOLATION :: $(cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 2>&1 | f)"
for t in e2e_proxy e2e_session e2e_runner e2e_actionlog e2e_bridge e2e_browser e2e_isolated_world; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
echo "CLI e2e_cli :: $(cargo test -p pegboard-cli --test e2e_cli -- --ignored --test-threads=1 2>&1 | f)"
oo() { cargo test -p pegboard-core --test e2e_policy -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
oc() { cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
F=crates/pegboard-core/src/downloads.rs; SE=crates/pegboard-core/src/session.rs; SV=crates/pegboard-core/src/service.rs
rep $F 'info["parentFrameId"].as_str().map(String::from)' 'None::<String>' && echo "M-b :: $(oo a_download_from_a_cross_site_frame_belongs_to_its_tabs_session)"; git checkout -q -- .
rep $SE '        inner.browser.downloads.own(&target, id);' '' && echo "M-c :: $(oo a_new_tab_that_only_downloads_is_its_sessions)"; git checkout -q -- .
rep $SE '                    inner.browser.downloads.own(&target, &id);' '' && echo "M-c2 :: $(oo a_new_tab_that_only_downloads_is_its_sessions)"; git checkout -q -- .
rep $F '                lost(&context);' '' && echo "M-e :: $(oc a_profile_whose_download_policy_stays_lost_loses_its_sessions)"; git checkout -q -- .
rep $SV '    async fn close_sessions_in(&self, context: &str) {' '    async fn close_sessions_in(&self, context: &str) {
        return;' && echo "M-e2 :: $(oc a_profile_whose_download_policy_stays_lost_loses_its_sessions)"; git checkout -q -- .
rep $F '            self.cancel_running(context, &staging).await;' '' && echo "M-f :: $(oo a_download_running_when_the_policy_drops_is_cancelled)"; git checkout -q -- .
rep $F '.filter(|(g, o)| g.as_str() != guid && o.end.is_none() && o.dropped.is_none())' '.filter(|_| false)' && echo "M-I1 :: $(oo parallel_downloads_over_the_session_limit_are_cut_while_running)"; git checkout -q -- .
rep $F '            (None, false) => self.agent_context_of(frame).await,' '            (None, false) => None,' '            let locate = running_dropped && d.context.is_none() && !human;' '            let locate = false;' && echo "M-g :: $(oo an_unattributed_running_download_is_cancelled)"; git checkout -q -- .
rep $F '            Ok(()) => send().await.map(drop),' '            Ok(()) => Err(anyhow::anyhow!("off")),' && echo "M-h :: $(oo a_limit_cancel_works_when_the_agent_profile_is_not_the_default)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DLLEFT :: $(ls ~/Downloads | grep -c -i peg)"
echo T5D_DONE
