This opt-in fixture checks frozen upgrade transport between two real source builds. It uses each build's `PROTOCOL` through the default signed handshake, a fresh loopback WebSocket, the existing fake Node and archive, and all five upgrade methods. It does not install a service, replace a running release, migrate a journal, or prove recovery of the same operation after a release switch.

Copy `cross_build_upgrade.rs` into both revisions' `tests/support/` and include it from `upgrade_mode.rs` with `#[path = "support/cross_build_upgrade.rs"] mod cross_build;`. Use a different `CARGO_TARGET_DIR` for each revision: Cargo can give both test binaries the same filename and a shared target directory would overwrite the older build. Build both `upgrade_mode` test binaries before starting either role and keep their artifacts unchanged until both roles exit. Use a fresh result directory for each pair, then run these commands concurrently from the respective worktrees:

```sh
CARGO_TARGET_DIR=/path/to/hub-build-target \
STEVE_CROSS_BUILD_DIR=/path/to/fresh-results STEVE_CROSS_BUILD_ROLE=hub \
  cargo nextest run -p steve-node-wire --test upgrade_mode --run-ignored only \
  -E 'test(cross_build::real_build_role)' --retries 0

CARGO_TARGET_DIR=/path/to/node-build-target \
STEVE_CROSS_BUILD_DIR=/path/to/fresh-results STEVE_CROSS_BUILD_ROLE=node \
  cargo nextest run -p steve-node-wire --test upgrade_mode --run-ignored only \
  -E 'test(cross_build::real_build_role)' --retries 0
```

Repeat with the build roles reversed. `hub-result.json` and `node-result.json` record the protocols actually announced, upgrade-only mode, the five-method coverage, archive reads and zero ordinary handler calls. Keys are ephemeral; only public keys are written. Default CI skips this test because it requires a second build. Nextest's reusable binaries/cargo metadata may be used to prevent rebuilding while the two peers are starting.
