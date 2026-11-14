export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
osascript -e 'set the clipboard to ""'
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "MISO :: $(cargo test -p pegboard-cli --test e2e_mcp_isolation -- --ignored --test-threads=1 2>&1 | f)"
echo "MISO_PLAIN :: $(cargo test -p pegboard-cli --test e2e_mcp_isolation 2>&1 | f)"
echo "MCP :: $(cargo test -p pegboard-cli --test e2e_mcp -- --ignored --test-threads=1 2>&1 | f)"
om() { cargo test -p pegboard-cli --test e2e_mcp_isolation -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|^error(\[|:)" | head -2 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
C=crates/pegboard-core/src; T=crates/pegboard-cli/src/mcp/tools.rs; TI=crates/pegboard-cli/tests/e2e_mcp_isolation.rs
rep $C/proxy/filter.rs '.is_some_and(|id| scope.targets.contains(id))' '.is_some()' && echo "R1 :: $(om mcp_agent_cannot_list_or_switch_to_human_tabs)"; git checkout -q -- .
python3 - <<'PY'
import re
p='crates/pegboard-cli/tests/e2e_mcp_isolation.rs'; s=open(p).read()
i=s.index('fn mcp_agent_cannot_read_human_cookies_or_storage'); j=s.index('/cookie/get',i)
k=s.rfind('\n',0,j)
ins='\n        a.ok("pegboard_open", json!({ "url": format!("{origin}/cookie/set") }));'
s=s[:k]+ins+s[k:]; open(p,'w').write(s)
PY
echo "R2(test-side) :: $(om mcp_agent_cannot_read_human_cookies_or_storage)"; git checkout -q -- .
rep $C/proxy/filter.rs 'Some(url) => ClientDecision::CreateTab { url },' 'Some(_) => ClientDecision::Forward,' && echo "R3 :: $(om mcp_new_tabs_open_in_the_agent_profile_with_the_human_window_last_active)"; git checkout -q -- .
rep $T 'None => return Err(no_session(id)),' 'None => call(ctx.core, "session.info_any", json!({ "sessionId": id }))?,' && echo "R4 :: $(om mcp_agent_cannot_reach_another_servers_session)"; git checkout -q -- .
rep $T 'operator: opts.operator,' 'operator: true,' && echo "R5 :: $(om mcp_agent_cannot_widen_its_policy)"; git checkout -q -- .
rep $C/address.rs 'if reaches_loopback(target) && own_ports.contains(&port) {' 'if false {' 'if loopback && own_ports.contains(&port) {' 'if false {' && echo "R7a :: $(om mcp_agent_cannot_reach_pegboards_own_ports_or_the_private_network)"; git checkout -q -- .
rep $C/address.rs 'if private && !policy.allow_private_network {' 'if false {' && echo "R7b :: $(om mcp_agent_cannot_reach_pegboards_own_ports_or_the_private_network)"; git checkout -q -- .
rep $C/policy.rs 'ActionClass::Upload | ActionClass::State | ActionClass::Clipboard' 'ActionClass::Upload | ActionClass::State' && echo "R8 :: $(om mcp_agent_cannot_write_the_clipboard)"; git checkout -q -- .
rep $C/downloads.rs '"behavior": "allowAndName",' '"behavior": "default",' && echo "R9 :: $(om mcp_download_lands_only_in_the_session_dir)"; git checkout -q -- .
rep $C/service.rs 'if self.stopping.load(Ordering::SeqCst) {' 'if true {' && echo "R10 :: $(om killed_mcp_server_leaves_sessions_that_expire)"; git checkout -q -- .
rep $C/upload.rs '(meta.file_type().is_file() && path_allowed(&path, std::slice::from_ref(dir)))' '(true)' && echo "R11 :: $(om mcp_upload_cannot_read_outside_its_upload_dir)"; git checkout -q -- .
rep $T 'let nonce = data_encoding::HEXLOWER.encode(&bytes);' 'let nonce = String::from("guess");' && echo "R12 :: $(om mcp_page_content_markers_wrap_page_text_and_a_page_cannot_end_the_block)"; git checkout -q -- .
rep $C/proxy/filter.rs '"Page.addScriptToEvaluateOnNewDocument",' '' && echo "R13 :: $(om mcp_clipboard_denied_session_can_still_run_vitals)"; git checkout -q -- .
rep $T 'owned.token(&id).ok_or_else(|| no_session(&id))?.to_string()' 'owned.token(&id).unwrap_or("").to_string()' && rep $C/service.rs 'Some(slot) if same => Ok(slot),' 'Some(slot) => Ok(slot),' && echo "R14/15 :: $(om mcp_server_cannot_drive_a_cli_session_it_did_not_adopt) $(om mcp_operator_mode_cannot_drive_other_sessions)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ') D $(pgrep -f 'pegboard (daemon|mcp)' | wc -l | tr -d ' ')"
echo "CLIPAFTER :: $(pbpaste | cut -c1-30)|"; osascript -e 'set the clipboard to ""'
echo T11A_DONE
