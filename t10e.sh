export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
for r in 1 2 3; do echo "MCP$r :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"; done
echo "MCP_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp 2>&1 | f)"
om() { cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
python3 - <<'PY'
p='crates/pegboard-cli/src/mcp/tools.rs'; s=open(p).read()
o='let id = SessionId::parse(text).ok_or_else(not_a_session_id)?;'; assert o in s
s=s.replace(o,'let id = SessionId::parse(text).ok_or_else(|| invalid(format!("{text:?} is not a session id")))?;',1); open(p,'w').write(s)
PY
echo "T9 :: $(om the_same_inputs_are_rejected_by_both)"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo T10E_DONE
