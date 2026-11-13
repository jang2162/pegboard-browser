export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR="$HOME/Projects/Personal/pegboard-browser/target"
echo "LEFTPROCS ::"; pgrep -fl 'user-data-dir' | grep -v -i playwright | sed -E 's#/Users/[^/]+#~#g' | awk '{print $2}' | sort | uniq -c | head
pgrep -fl 'user-data-dir' | grep -o -- '--user-data-dir=[^ ]*' | sed -E 's#/Users/[^/]+#~#g' | sort | uniq -c | head
for t in redirect_to_private_address_is_blocked allow_domain_closes_everything_else_including_localhost new_tabs_and_popups_are_guarded_before_their_first_request; do
 echo "== $t"; cargo test -p pegboard-cli --test e2e_policy $t -- --ignored --test-threads=1 --exact 2>&1 | sed -n '/panicked at/,/^note:/p' | sed -E 's#/Users/[^/]+#~#g' | cut -c1-400 | head -15
done
echo T4B_DONE
