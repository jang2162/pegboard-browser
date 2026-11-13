export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
cd "$1" && git checkout -q -- . && git fetch -q origin && git checkout -q --detach origin/phase2a-mcp-policy && git log --oneline -1 | cut -c1-60
f() { grep -E "^test .*FAILED|^test result|^error(\[|:)|panicked at" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-240 | tr '\n' ' '; }
echo "PROXY :: $(cargo test -p pegboard-core --test e2e_proxy -- --ignored --test-threads=1 2>&1 | f)"
echo "POLICY :: $(cargo test -p pegboard-cli --test e2e_policy -- --ignored --test-threads=1 2>&1 | f)"
one() { cargo test -p pegboard-core --test e2e_proxy set_file_input_files -- --ignored --test-threads=1 2>&1 | grep -E "^test .*(ok|FAILED)$|panicked at|^error(\[|:)" | sed -E 's#/Users/[^/]+#~#g' | cut -c1-200 | head -3 | tr '\n' ' '; }
rep() { python3 - "$@" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read(); pairs=sys.argv[2:]
for old,new in zip(pairs[::2],pairs[1::2]):
    if old not in s: print("MUTATION TARGET NOT FOUND: "+old[:70]); sys.exit(1)
    s=s.replace(old,new,1)
open(p,'w').write(s)
PY
}
C=crates/pegboard-core/src
rep $C/session.rs 'if session_policy.allow.contains(&ActionClass::Upload) {' 'if true {' && echo "F1 :: $(one)"; git checkout -q -- .
rep $C/session.rs 'Some(inner.paths.session_files(&id)),' 'Some(PathBuf::from("/")),' && echo "F2 :: $(one)"; git checkout -q -- .
rep $C/proxy/filter.rs 'crate::upload::allowed_real_path(Path::new(f), &scope.upload_dirs))' 'Some(PathBuf::from(f)))' && echo "F3 :: $(one)"; git checkout -q -- .
git status --short | head -2; echo T6B_DONE
