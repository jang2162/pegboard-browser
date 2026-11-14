export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CORE_POLICY :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI_ISOLATION :: $(cargo test -p pegboard-cli --test e2e_isolation -- --ignored --test-threads=1 2>&1 | f)"
python3 - <<'PY'
p='crates/pegboard-core/src/downloads.rs'; s=open(p).read()
o='Err(e) => e,'; assert s.count(o)>=1
i=s.index(o); s=s[:i+len(o)]+' self.activate(context).await.ok();'+s[i+len(o):]
open(p,'w').write(s)
PY
echo "MU_focus :: $(cargo test -p pegboard-core --test e2e_policy -- --ignored --exact a_limit_cancel_works_when_the_agent_profile_is_not_the_default --test-threads=1 2>&1 | grep -E '^test .*(ok|FAILED)$|panicked|^error(\[|:)|activated' | head -3 | cut -c1-200 | tr '\n' ' ')"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DLLEFT :: $(ls ~/Downloads | grep -c -i peg)"
echo T5F_DONE
