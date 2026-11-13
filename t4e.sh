export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at|^  left|^ right" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CORE_POLICY :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
for t in e2e_proxy e2e_session e2e_runner e2e_actionlog e2e_bridge e2e_browser e2e_isolated_world; do echo "CORE $t :: $(cargo test -p pegboard-core --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
for t in e2e_cli e2e_isolation; do echo "CLI $t :: $(cargo test -p pegboard-cli --test $t -- --ignored --test-threads=1 2>&1 | f)"; done
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"
echo T4E_DONE
