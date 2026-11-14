export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "MCP :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
echo "MCP2 :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
echo "== CLI markers"; cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact page_text_carries_boundary_markers --nocapture 2>&1 | grep -i -E 'cli|marker|AGENT_BROWSER' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -12
python3 - <<'PY'
p='crates/pegboard-cli/src/mcp/tools.rs'; s=open(p).read()
o='if ok && (page_content || boundary_of(&r.output).is_some()) {'; assert o in s; s=s.replace(o,'if false {',1); open(p,'w').write(s)
PY
echo "T8 :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact page_text_carries_boundary_markers --test-threads=1 2>&1 | grep -E '^test .*(ok|FAILED)$|^error(\[|:)' | head -2 | tr '\n' ' ')"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo T10C_DONE
