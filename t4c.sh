export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at|^  left|^ right" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-260; }
echo "== ALL"; cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f
echo "== SERVED_AGAIN"; cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact served_again_page_on_an_insecure_origin_reaches_allowed_local_hosts --nocapture 2>&1 | grep -v -E "^\s*(Compiling|Finished|Running)" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-260 | tail -25
echo T4C_DONE
