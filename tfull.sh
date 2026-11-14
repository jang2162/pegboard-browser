export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
osascript -e 'set the clipboard to ""'
echo "FMT :: $(cargo fmt --check >/dev/null 2>&1 && echo ok || echo FAIL)"
echo "CLIPPY :: $(cargo clippy -q --workspace --tests -- -D warnings 2>&1 | tail -1)ok?"
echo "UNIT :: $(cargo test --workspace 2>&1 | grep -E '^test result' | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}')"
cargo test --workspace --no-fail-fast -- --ignored --test-threads=1 > /tmp/peg-full.log 2>&1
echo "FULL :: $(grep -E '^test result' /tmp/peg-full.log | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}')"
grep -E '^test .*FAILED|panicked at' /tmp/peg-full.log | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -20
echo "VITALS ROW :: $(cargo test -p pegboard-core --test e2e_runner -- --ignored --test-threads=1 --nocapture --exact compatibility_matrix 2>&1 | grep -E 'ROW.*vitals' | cut -c1-200)"
python3 - <<'PY'
p='crates/pegboard-core/src/proxy/filter.rs'; s=open(p).read(); o='if PAGE_SCRIPT_METHODS.contains(&method) && !scope.page_scripts_allowed {'; assert o in s; s=s.replace(o,'if false {',1); open(p,'w').write(s)
PY
echo "K3MUT :: $(cargo test -p pegboard-core --test e2e_clipboard -- --ignored --exact agent_page_scripts_are_refused_unless_the_clipboard_is_allowed --test-threads=1 2>&1 | grep -E '^test .*(ok|FAILED)$|^error' | head -2 | tr '\n' ' ')"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ') DL $(ls ~/Downloads | grep -c -i peg)"
echo "CLIPAFTER :: $(pbpaste | cut -c1-30)|"; osascript -e 'set the clipboard to ""'
echo TFULL_DONE
