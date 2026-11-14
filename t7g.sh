export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
osascript -e 'set the clipboard to ""'
echo "CLIPBEFORE :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-120)"
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CLIP :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1 2>&1 | f)"
echo "CLIPMID :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-80) | $(pbpaste | cut -c1-30)"
echo "== DOCOPEN"; cargo test -p pegboard-core --test e2e_clipboard -- --ignored --exact document_open_and_write_do_not_remove_the_shield --nocapture 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running)|pegboard guard: document' | grep -i -E 'allow=|shield first|page first|realm|panicked' | cut -c1-200 | head -30
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
G=crates/pegboard-core/src/guard.rs
rep $G '"Page.documentOpened" => self.on_document_opened(conn, &event),' '"Page.documentOpened" => {},' && echo "MC1 :: $(oc the_guard_renews_a_document_opened_past_the_shield)"; git checkout -q -- .
rep $G 'Ok((true, seen)) if again.is_ok() =>' 'Ok((_, seen)) =>' && echo "MC2 :: $(oc the_guard_renews_a_document_opened_past_the_shield)"; git checkout -q -- .
rep $G 'json!({ "objectId": window })' 'json!({ "objectId": doc })' && echo "MC3 :: $(oc document_open_and_write_do_not_remove_the_shield)"; git checkout -q -- .
rep $G 'ap(typeof g==="function"?g:on,undefined,[d])' 'ap(on,undefined,[d])' && echo "MC4 :: $(oc document_open_and_write_do_not_remove_the_shield)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo "CLIPAFTER :: $(osascript -e 'clipboard info' 2>&1 | cut -c1-80) | $(pbpaste | cut -c1-30)"
osascript -e 'set the clipboard to ""'
echo T7G_DONE
