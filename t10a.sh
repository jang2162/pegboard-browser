export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "MCP :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
echo "MCP_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp 2>&1 | f)"
echo "CLI e2e_cli :: $(cargo test -p pegboard-cli --test e2e_cli -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI e2e_isolation :: $(cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 2>&1 | f)"
om() { cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
T=crates/pegboard-cli/src/mcp/tools.rs; D=crates/pegboard-core/src/daemon.rs
rep $T '                out.push("--".to_string());' '' && echo "T1 :: $(om the_same_inputs_are_rejected_by_both) $(om secrets_typed_over_mcp_are_redacted_like_the_cli)"; git checkout -q -- .
rep $T '        let spelled = flag.names[0];' '        let spelled = flag.names[flag.names.len() - 1];' && echo "T2 :: $(om cli_and_mcp_run_the_same_command_the_same_way)"; git checkout -q -- .
rep $T '        crate::render_output(&r.output)' '        r.output.to_string()' && echo "T3 :: $(om cli_and_mcp_run_the_same_command_the_same_way)"; git checkout -q -- .
rep $T '    if ok && word == "screenshot" {' '    if false {' && echo "T4 :: $(om screenshot_returns_an_image)"; git checkout -q -- .
rep $T '    for line in crate::download_lines(&r.output) {' '    for line in Vec::<String>::new() {' && echo "T5 :: $(om downloads_are_told_in_mcp_results)"; git checkout -q -- .
rep $D '        .and_then(|class| policy::judge_action(&sessions.policy(id)?, class));' '        .map(|_| ());' && echo "T6 :: $(om the_session_policy_applies_to_mcp_actions)"; git checkout -q -- .
rep $D '        types_text && !(harmless_before && succeeded && !probe.failed && !probe.sensitive);' '        false;' && echo "T7 :: $(om secrets_typed_over_mcp_are_redacted_like_the_cli)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo T10A_DONE
