export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "== MEASURE"
cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 --nocapture measure_making_the_agent_profile_default_without_focus a_limit_cancel_works_when_the_agent_profile_is_not_the_default a_new_tab_that_only_downloads_is_its_sessions 2>&1 | grep -E 'MEASURE|cancels|survive|tab|panicked|^test |test result' | grep -v -E '^\s*Running' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-260 | head -30
echo "CORE_POLICY :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI_ISOLATION :: $(cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 2>&1 | f)"
for t in e2e_proxy e2e_session e2e_runner e2e_actionlog; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
echo "CLI e2e_cli :: $(cargo test -p pegboard-cli --test e2e_cli -- --ignored --test-threads=1 2>&1 | f)"
oo() { cargo test -p pegboard-core --test e2e_policy -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
F=crates/pegboard-core/src/downloads.rs; SE=crates/pegboard-core/src/session.rs
T=a_new_tab_that_only_downloads_is_its_sessions
rep $SE '                inner.browser.downloads.own(target_id, id);' '' && echo "MU1 (expect pass) :: $(oo $T)"; git checkout -q -- .
rep $SE '                inner.browser.downloads.own(target_id, id);' '' '        inner.browser.downloads.own(&target, id);' '' '                    inner.browser.downloads.own(&target, &id);' '' && echo "MU2 (expect fail) :: $(oo $T)"; git checkout -q -- .
rep $F 'const RESOLVE_TRIES: u32 = 4;' 'const RESOLVE_TRIES: u32 = 1;' && rep $SE '                    inner.browser.downloads.own(&target, &id);' '' && echo "MU3 :: $(oo $T)"; git checkout -q -- .
rep $F '        let again = match self.make_default_quietly(context).await {' '        let again = match { self.activate(context).await.ok(); self.make_default_quietly(context).await } {' && echo "MU_focus :: $(oo a_limit_cancel_works_when_the_agent_profile_is_not_the_default)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DLLEFT :: $(ls ~/Downloads | grep -c -i peg)"
echo T5E_DONE
