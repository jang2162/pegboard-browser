export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-220 | tr '\n' ' '; }
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "PUBLIC_LOADS :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact inert_documents_get_the_csp_too --nocapture 2>&1 | grep -E '^(public page|local page)' | cut -c1-160 | tr '\n' ' ')"
echo "BYTES :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact served_again_bodies_keep_their_bytes --nocapture 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running)' | grep -i -E 'sjis|utf8|public|localhost|panicked|test result' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | tr '\n' ' ')"
echo "CORE_POLICY :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
for t in e2e_proxy e2e_session e2e_runner e2e_actionlog e2e_bridge e2e_browser e2e_isolated_world; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
for t in e2e_cli e2e_isolation; do echo "CLI $t :: $(cargo test -p pegboard-cli --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
oc() { cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
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
G=crates/pegboard-core/src/guard.rs; D=crates/pegboard-core/src/daemon.rs; H=crates/pegboard-core/tests/common/mod.rs
rep $G '            | "application/x-7z-compressed"
    )' '            | "application/x-7z-compressed"
            | "text/plain"
    ) || mime.starts_with("image/")' && echo "M_inert :: $(oc inert_documents_get_the_csp_too)"; git checkout -q -- .
rep $G 'if !serves_again(stored.as_ref(), policy, &own_ports) {' 'if true {' && echo "M_noserve :: $(oo framed_documents_of_a_public_page_get_the_csp)"; git checkout -q -- .
rep $H '            format!("127.0.0.1:{public_port}=public"),' '            String::new(),' && echo "M_fixture :: $(oo framed_documents_of_a_public_page_get_the_csp)"; git checkout -q -- .
rep $G ".flat_map(|(_, v)| v.split(','))" ".flat_map(|(_, v)| v.split(',')).take(1)" && echo "M_dupct :: $(oc sniffed_and_duplicated_content_types_get_the_csp)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"
echo T4H_DONE
