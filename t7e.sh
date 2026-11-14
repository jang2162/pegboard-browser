export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
echo "CLIPBEFORE :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CLIP :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1 2>&1 | f)"
echo "DOCOPEN :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --exact document_open_and_write_do_not_remove_the_shield --nocapture 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running)|pegboard guard:' | grep -i -E 'allow|den|copied|changed|panicked|result' | cut -c1-160 | head -14 | tr '\n' ' ')"
echo "CLIPMID :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
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
C=crates/pegboard-core/src; G=$C/guard.rs
rep $G 'ap(pd,e,[]);ap(sip,e,[])' 'ap(pd,e,[])' && echo "MS1 :: $(oc agent_cannot_write_the_clipboard_by_default)"; git checkout -q -- .
rep $G 'const T=[ap(dv,d,[]),d]' 'const T=[d,d]' && echo "MS2 :: $(oc sandboxed_srcdoc_frame_listening_first)"; git checkout -q -- .
rep $G 'const FRAME_RELOADS: u32 = 3;' 'const FRAME_RELOADS: u32 = 0;' && echo "MS3 :: $(oc many_and_returning_sandboxed_frames_stay_covered) $(oc sandboxed_srcdoc_frame_listening_first)"; git checkout -q -- .
rep $G 'const r=ap(op,this,arguments);on(this);return r' 'const r=ap(op,this,arguments);return r' && echo "MS4 :: $(oc document_open_and_write_do_not_remove_the_shield)"; git checkout -q -- .
rep $G 'if(ap(rs,d,[])!=="loading")' 'if(false)' && echo "MS5 :: $(oc document_open_and_write_do_not_remove_the_shield)"; git checkout -q -- .
rep $C/session.rs '&& !(denied && self.inner.guard.early_pending(&context))' '&& !(denied && false)' && echo "MS6 :: $(oc commands_wait_for_the_old_tabs_after_a_reconnect)"; git checkout -q -- .
rep $G 'let closed = self.close_tab_of(conn, session).await;' 'let closed = String::new();' && echo "MS7 :: $(oc a_frame_whose_shield_fails_closes_its_tab)"; git checkout -q -- .
rep $C/downloads.rs '} else if policy.clipboard_maybe_denied {' '} else if false && policy.clipboard_maybe_denied {' && echo "MS8 :: $(oc a_lost_denial_is_lifted_for_a_session_that_allows_the_clipboard)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo "CLIPAFTER :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120) | $(pbpaste | cut -c1-30)"
echo T7E_DONE
