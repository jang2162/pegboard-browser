export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-220 | tr '\n' ' '; }
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
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
G=crates/pegboard-core/src/guard.rs; D=crates/pegboard-core/src/daemon.rs
rep $G '            | "application/x-7z-compressed"
    )' '            | "application/x-7z-compressed"
            | "text/plain"
    ) || mime.starts_with("image/")' && echo "M_inert :: $(oc inert_documents_get_the_csp_too)"; git checkout -q -- .
rep $G 'if body.len() > limit {' 'if false {' && echo "M_cap :: $(oo oversized_documents_fail_and_the_guard_stays_up)"; git checkout -q -- .
rep $G ".flat_map(|(_, v)| v.split(','))" ".take(1).flat_map(|(_, v)| v.split(','))" && echo "M_dupct :: $(oc sniffed_and_duplicated_content_types_get_the_csp)"; git checkout -q -- .
rep $G 'if params["resourceType"] == "Document" {' 'if false {' && echo "M_store :: $(oc pages_on_allowed_local_origins_are_not_served_again)"; git checkout -q -- .
rep $G '        let result = async {
            if matches!(kind, "page" | "iframe") {' '        let result = async {
            if false {' && echo "M_oopif :: $(oc oopif_subrequests_are_judged)"; git checkout -q -- .
rep $D '    if let Err(e) = sessions.require_guard() {
        return refuse(log, entry, argv, e, started);
    }' '' && echo "M_guarddown :: $(oo commands_are_refused_while_the_guard_is_down)"; git checkout -q -- .
rep $G 'let foreign = context == self.human_context || bridge_worker;' 'let foreign = bridge_worker;' && echo "M_human :: $(oo the_human_profile_is_untouched)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"
echo T4G_DONE
