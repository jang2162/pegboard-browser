export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
om() { cargo test -p pegboard-cli --test e2e_mcp_isolation -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
ev() { echo "  ROW: $(cargo test -p pegboard-core --test e2e_runner -- --ignored --test-threads=1 --nocapture --exact compatibility_matrix 2>&1 | grep -E 'ROW.*vitals' | cut -c1-300)"; echo "  MCP: $(cargo test -p pegboard-cli --test e2e_mcp_isolation -- --ignored --test-threads=1 --nocapture --exact mcp_clipboard_denied_session_can_still_run_vitals 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running)|pegboard guard' | grep -i -E 'vitals|panicked|^test ' | cut -c1-220 | head -6 | tr '\n' ' ')"; }
echo "== HEAD"; ev
python3 - <<'PY'
p='crates/pegboard-core/src/proxy/filter.rs'; s=open(p).read(); o='"Page.addScriptToEvaluateOnNewDocument",'; assert o in s; s=s.replace(o,'',1); open(p,'w').write(s)
PY
echo "== R13"; ev; git checkout -q -- .
python3 - <<'PY'
p='crates/pegboard-core/src/proxy/filter.rs'; s=open(p).read(); o='.is_some_and(|id| scope.targets.contains(id));'
assert s.count(o)>=1; s=s.replace(o,'.is_some();'); open(p,'w').write(s)
o2='.is_some_and(|id| scope.targets.contains(id))'
s=s.replace(o2,'.is_some()'); open(p,'w').write(s)
PY
echo "R1 both :: $(om mcp_agent_cannot_list_or_switch_to_human_tabs)"; git checkout -q -- .
python3 - <<'PY'
p='crates/pegboard-cli/tests/e2e_mcp_isolation.rs'; s=open(p).read()
i=s.index('fn mcp_agent_cannot_read_human_cookies_or_storage'); j=s.index('} = &mut rig;',i)+len('} = &mut rig;')
s=s[:j]+'\n    a.ok("pegboard_open", json!({ "url": format!("{origin}/cookie/set") }));'+s[j:]; open(p,'w').write(s)
PY
echo "R2 :: $(om mcp_agent_cannot_read_human_cookies_or_storage)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo T11B_DONE
