export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
h() { cargo test -p pegboard-core --test e2e_policy -- --ignored --exact the_human_profile_is_untouched --nocapture 2>&1 | grep -v -E '^\s*(Compiling|Finished|Running|warning)' | grep -i -E 'control|guard|popup|panicked|test result|released|human' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-220 | head -12; }
echo "== RUN1"; h; echo "== RUN2"; h
echo "== CORE_POLICY_ALL :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | grep -E '^test result|FAILED' | tr '\n' ' ')"
python3 - <<'PY'
p='crates/pegboard-core/src/guard.rs'; s=open(p).read()
old='''    let resumed = conn
        .send("Runtime.runIfWaitingForDebugger", json!({}), Some(session))
        .await;'''
assert old in s, "MUT NOT FOUND"
s=s.replace(old,'''    let resumed: Result<Value> = Err(anyhow::anyhow!("skipped"));''',1); open(p,'w').write(s)
PY
echo "== MUTATION (no resume)"; h; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"
echo T4F_DONE
