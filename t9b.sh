export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "MCP :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
echo "MCP_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp 2>&1 | f)"
om() { cargo test -p pegboard-cli --test e2e_mcp -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
M=crates/pegboard-cli/src/mcp/mod.rs
rep $M '_ = stops.next(true) => { in_flight = Some(task); break }' '_ = stops.int.recv() => { in_flight = Some(task); break }' && echo "N2 :: $(om sigterm_during_a_running_call_ends_the_server) $(om stdin_end_during_a_running_call_ends_the_server)"; git checkout -q -- .
rep $M '        _ = stops.int.recv() => 130,' '        _ = std::future::pending::<()>() => 130,' && echo "N3 :: $(om a_second_sigint_ends_at_once)"; git checkout -q -- .
rep $M '            _ = stops.next(false) => break,' '            _ = stops.term.recv() => break,' && echo "N4 :: $(om sessions_are_closed_on_sigint_and_sighup)"; git checkout -q -- .
rep $M '                sent = out.send(reply) => if sent.is_err() { break },
                _ = stops.next(true) => break,' '                sent = out.send(reply) => if sent.is_err() { break },' && echo "N5 :: $(om a_client_that_stops_reading_still_ends_on_sigterm)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DAEMONS :: $(pgrep -f 'pegboard daemon' | wc -l | tr -d ' ')"; echo "MCPPROC :: $(pgrep -f 'pegboard mcp' | wc -l | tr -d ' ')"
echo T9B_DONE
