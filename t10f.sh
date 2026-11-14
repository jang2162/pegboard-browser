export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
for r in 1 2; do echo "MCP$r :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"; done
echo "MCP_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp 2>&1 | f)"
echo "CLI e2e_isolation :: $(cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 2>&1 | f)"
om() { cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
python3 - <<'PY'
p='crates/pegboard-cli/src/mcp/tools.rs'; s=open(p).read()
o='t => wrap_page_content(&t, origin.as_deref()),'; assert o in s; s=s.replace(o,'t => t,',1); open(p,'w').write(s)
PY
echo "T8 :: $(om page_text_carries_boundary_markers)"; git checkout -q -- .
python3 - <<'PY'
p='crates/pegboard-cli/src/mcp/tools.rs'; s=open(p).read()
o='wrap_page_content(&crate::failure_reason(&r), origin.as_deref())'; assert o in s; s=s.replace(o,'crate::failure_reason(&r)',1); open(p,'w').write(s)
PY
echo "T10 :: $(om page_text_carries_boundary_markers)"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo T10F_DONE
