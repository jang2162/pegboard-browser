export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-50
cargo build -q -p pegboard-cli 2>&1 | tail -2
export PATH="$CARGO_TARGET_DIR/debug:$PATH"
export PEGBOARD_HOME=$(mktemp -d); echo "bin: $(command -v pegboard | sed -E 's#/Users/[^/]+#~#')"
r() { echo "\$ $*"; "$@" 2>&1 | sed -E 's#/Users/[^/]+#~#g; s/([A-Z2-7]{26})/<token>/g' | cut -c1-140 | head -6; echo "  exit=${PIPESTATUS[0]}"; }
r pegboard browser start
H=$(pegboard session create --name "조사" --url https://example.com 2>&1 | head -1); echo "\$ pegboard session create ... -> $(echo "$H" | sed -E 's/\.[A-Z2-7]{26}/.<token>/')"
export PEGBOARD_SESSION="$H"
r pegboard snapshot -i
r pegboard click @e1
r pegboard get url
r pegboard eval "document.title"
r pegboard files list
r pegboard session info "$PEGBOARD_SESSION"
r pegboard session list
RID=$(pegboard recording list 2>/dev/null | awk 'NR==1{print $1}'); r pegboard recording list
r pegboard recording show "$RID"
r pegboard session close "$PEGBOARD_SESSION"
unset PEGBOARD_SESSION
r pegboard profile create work
W=$(pegboard session create --name 업무 --profile work 2>&1 | head -1); echo "\$ session create --profile work -> $(echo "$W" | sed -E 's/\.[A-Z2-7]{26}/.<token>/')"
r pegboard profile list
r pegboard profile delete work
r pegboard session close "$W"
r pegboard profile delete work
echo "== MCP"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pegboard_session_create","arguments":{"url":"https://example.com"}}}' | pegboard mcp --allow-domain example.com 2>&1 | sed -E 's/[A-Z2-7]{26}/<token>/g' | cut -c1-200
echo "  exit=$?"
r pegboard session list
r pegboard browser stop
rm -rf "$PEGBOARD_HOME"; echo QS_DONE
