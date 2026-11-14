export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
T='-p pegboard-cli --test e2e_mcp_isolation -- --ignored --test-threads=1 --exact mcp_session_closed_under_allow_domain_leaves_no_tab_outside_it'
for i in 1 2; do echo "RUN$i :: $(cargo test $T 2>&1 | grep -E '^test .*(ok|FAILED)$' | tr '\n' ' ')"; done
python3 - <<'PY'
p='crates/pegboard-core/src/session.rs'; s=open(p).read()
start=s.index('        let SessionRecord {\n            mut info,'); end=s.index('inner.release_pin(id, pin, &bridge, &group, &targets).await;\n',start)+len('inner.release_pin(id, pin, &bridge, &group, &targets).await;\n')
blk=s[start:end]; s=s[:start]+s[end:]
r=s.index('        let result = if p.keep_tabs {'); s=s[:r]+blk+s[r:]
e=s.index('result.map_err',r); seg=s[r:e].replace('record.bridge','bridge'); s=s[:r]+seg+s[e:]
open(p,'w').write(s)
PY
echo "MUT :: $(cargo test $T 2>&1 | grep -E '^test .*(ok|FAILED)$|^error(\[|:)|left: |right: ' | head -4 | tr '\n' ' ')"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo TPIN_DONE
