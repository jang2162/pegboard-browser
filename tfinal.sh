export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
osascript -e 'set the clipboard to ""'
echo "FMT :: $(cargo fmt --check >/dev/null 2>&1 && echo ok || echo FAIL)"
echo "CLIPPY :: $(cargo clippy -q --workspace --tests -- -D warnings 2>&1 | tail -1)ok?"
echo "UNIT :: $(cargo test --workspace 2>&1 | grep -E '^test result' | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}')"
cargo test --workspace --no-fail-fast -- --ignored --test-threads=1 > /tmp/peg-full.log 2>&1
echo "FULL :: $(grep -E '^test result' /tmp/peg-full.log | awk '{p+=$4; f+=$6} END {print p" passed, "f" failed"}')"
grep -E '^test .*FAILED|panicked at' /tmp/peg-full.log | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -20
echo "GETURL :: $(H=$(mktemp -d); export PEGBOARD_HOME=$H; ./target/debug/pegboard browser start >/dev/null 2>&1 || $CARGO_TARGET_DIR/debug/pegboard browser start >/dev/null 2>&1; P=$CARGO_TARGET_DIR/debug/pegboard; S=$($P session create --url data:text/html,%3Ctitle%3ET%3C/title%3E 2>/dev/null); export PEGBOARD_SESSION=$S; echo "[$($P get url 2>&1 | head -2 | tr '\n' ' ')] [$($P get title 2>&1 | head -1)] [$($P eval 1+1 2>&1 | head -1)]"; $P browser stop >/dev/null 2>&1; rm -rf $H)"
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ') DL $(ls ~/Downloads | grep -c -i peg)"
echo "CLIPAFTER :: $(pbpaste | cut -c1-30)|"; osascript -e 'set the clipboard to ""'
echo TFINAL_DONE
