export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "${1:-$PWD}" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "CLI_POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
F=crates/pegboard-core/src/downloads.rs
python3 - <<'PY'
p='crates/pegboard-core/src/downloads.rs'; s=open(p).read()
for o,n in [('(Some(_), Some(v)) if !v.allows_downloads =>','(Some(_), Some(v)) if false && !v.allows_downloads =>'),('Some(v)) if v.allows_downloads => match staged {','Some(v)) if true || v.allows_downloads => match staged {')]:
    assert o in s, o; s=s.replace(o,n,1)
open(p,'w').write(s)
PY
echo "M2' :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 --exact deny_download_writes_no_file_anywhere 2>&1 | f)"; git checkout -q -- .
echo "LEFT :: $(pgrep -f -- '--user-data-dir=/(private/)?var/folders' | wc -l | tr -d ' ')"; echo "DLLEFT :: $(ls ~/Downloads | grep -c -i peg)"
echo T5C_DONE
