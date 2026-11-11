# Task 1 report

Status: DONE

Implemented: workspace (Cargo.toml, crates pegboard-protocol and pegboard-core), protocol types per brief (plus a `codes` module of error-code constants and Debug/Clone/PartialEq derives), `paths()`/`ensure()`, LICENSE (MIT, "pegboard-browser contributors"), THIRD_PARTY.md, scripts/fetch-agent-browser.sh, vendor/agent-browser.lock, .gitignore (+ vendor/agent-browser/).

Decisions: Paths.state = home/state.json; ensure() also chmods pre-existing dirs to 0700 (needed: tempdir is created 0755, test failed first without it). Added libc dep for getuid. LICENSE holder avoids personal name.

Tarball: darwin-arm64 binary is at package/bin/agent-browser-darwin-arm64 in the npm tarball (as the brief says); sha256 b1b2946c95d218b035262483f2c6193e0f216c155c903bcc532ce47e29d8e72d. Script installs it as vendor/agent-browser/agent-browser.

TDD: RED = `cargo test -p pegboard-protocol -p pegboard-core` failed to compile (SessionId etc. undefined); after first implementation the paths test failed (mode 493 != 448) because the tempdir pre-existed, fixed with set_permissions. GREEN: 4 tests pass (3 protocol, 1 core), no warnings.
Script check: first run recorded sha, `agent-browser 0.42.0`, second run `checksum ok`.

Concern: only the paths test sets PEGBOARD_HOME; it is the only test in pegboard-core's binary for now.
