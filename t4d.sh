export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
echo "DNS :: $(dscacheutil -q host -a name example.net | awk '/ip_address/{print "resolves"; exit}')"
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at|^  left|^ right" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "ALL :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
one() { cargo test -p pegboard-cli --test e2e_policy -- --ignored --exact "$1" --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|panicked at|^error(\[|:)" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -3 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
G=crates/pegboard-core/src/guard.rs
L='    !local_and_allowed(addresses.as_deref(), &host, port, policy, own_ports)'
rep $G "$L" '    true' && echo "M1 local->served :: $(one pages_on_allowed_local_origins_are_not_served_again)"; git checkout -q -- .
rep $G "$L" '    false' && echo "M2 public->not served :: $(one pages_from_a_public_address_get_the_csp)"; git checkout -q -- .
rep $G "$L" '    false' && echo "M3 private ws :: $(one private_network_is_closed_unless_allowed)"; git checkout -q -- .
rep $G '    if policy.allow_domains.is_empty() {' '    if true {' && echo "M4 allow csp :: $(one allow_domain_closes_everything_else_including_localhost)"; git checkout -q -- .
git status --short | head -2; echo "LEFT :: $(pgrep -f 'user-data-dir=.*(pegboard|\.tmp)' | wc -l | tr -d ' ')"
echo T4D_DONE
