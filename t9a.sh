export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "MCP :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
echo "MCP_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp 2>&1 | f)"
echo "CLI e2e_cli :: $(cargo test -p pegboard-cli --test e2e_cli -- --ignored --test-threads=1 2>&1 | f)"
echo "CLI e2e_policy :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
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
M=crates/pegboard-cli/src/mcp
rep $M/mod.rs '.filter(|(_, h)| h.created_here)' '.filter(|_| true)' && echo "E1 :: $(om adopted_session_keeps_its_own_wider_policy) $(om sessions_are_closed_when_stdin_closes)"; git checkout -q -- .
rep $M/mod.rs 'close_created(&owned, &connect);' 'let _ = (&owned, &connect);' && echo "E2 :: $(om sessions_are_closed_when_stdin_closes) $(om sessions_are_closed_on_sigterm)"; git checkout -q -- .
rep $M/mod.rs '            _ = term.recv() => break,' '' && echo "E3 :: $(om sessions_are_closed_on_sigterm)"; git checkout -q -- .
rep $M/tools.rs '_ => self.policy.clone(),' '_ => SessionPolicy::default(),' && echo "E4 :: $(om server_policy_is_attached_to_every_session)"; git checkout -q -- .
rep $M/tools.rs '.filter(|s| self.operator || owned.token(&s.id).is_some())' '.filter(|_| true)' && echo "E5 :: $(om default_mode_cannot_see_or_touch_another_servers_session)"; git checkout -q -- .
python3 - <<'PY'
p='crates/pegboard-core/src/service.rs'; s=open(p).read()
i=s.index('"session.close_any" =>'); j=s.index('let slot = self.open_slot(&id)?;', i)
s=s[:j]+'let slot = self.authorize(&id, None)?;'+s[j+len('let slot = self.open_slot(&id)?;'):]
open(p,'w').write(s)
PY
echo "E6 :: $(om operator_mode_lists_and_closes_but_does_not_drive_other_sessions)"; git checkout -q -- .
rep $M/mod.rs '            start: Start::Pending(std::thread::spawn(move || crate::browser_start(&p))),' '            start: { let _ = p; Start::Done },' && echo "E7 :: $(om mcp_starts_the_daemon_and_creates_a_session)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DAEMONS :: $(pgrep -f 'pegboard daemon' | wc -l | tr -d ' ')"
echo T9A_DONE
