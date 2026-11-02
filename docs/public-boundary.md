# Public distribution boundary

Steve's source is licensed under MIT. Third-party dependencies retain their own licenses; adding this license does not complete a dependency or distribution audit.

The public build must not depend on private repositories, private service availability, personal accounts or private filesystem locations. Private deployment assemblies depend on public APIs; the public code must not depend on the private assembly.

```text
Private configuration / adapters
              |
              v
       Public extension APIs
              |
              v
         Public domains
```

## Checks

Run with Python 3.11 or newer:

```sh
python3.13 scripts/test-public-boundary.py
python3.13 scripts/check-public-boundary.py
python3.13 scripts/check-public-boundary.py --strict
```

The default check rejects external/absolute Cargo path dependencies, unreviewed Git dependencies, custom registries and new personal identifiers in application source, fixtures and deployment scripts. It checks dependencies under target-specific tables and Cargo patch/replace tables as well as Cargo.lock. Registry dependencies on crates.io remain subject to a separate license audit.

The checked-in baseline is empty. `--strict` scans all tracked text (including docs and fixtures); only the scanner's detection definitions and adversarial unit tests are excluded. Matching content is fingerprinted, never printed. The strict check passes after replacing personal examples with reserved domains and neutral distribution identities. Local `.steve/` artifacts are excluded from future commits, and historical QA screenshots have been removed from this source snapshot. The production permission-request fixture has been replaced by 16 synthetic commands.

This lexical check is not a comprehensive secret scanner and does not inspect Git history, binaries or runtime authorization. Public distribution still requires an independent dependency-license audit, integrated clean build, and publication from the separately authorized clean history. Test-only RSA keys and secret-redaction examples must not be used in deployments.

## Model configuration

There is no required private gateway in `catalog.rs`. Claude Code models already come from the user's `modelPicker.options` in its settings; Codex models come from `codex debug models`. The existing `STEVE_MODELS_FAST`, `STEVE_MODELS_SMART` and `STEVE_DEFAULT_MODEL_CLAUDE` settings configure private choices without an additional preset mechanism. Unrelated keys in Claude settings are not returned or executed. Private routes may appear in user configuration; a private gateway name in a test description does not establish a runtime dependency.

## Keyhelper distribution identity

The keychain service and signed caller identifiers are build inputs, not runtime overrides. A distributor can compile the helper with its own identifiers:

```sh
STEVE_KEYHELPER_SERVICE=org.example.steve.vault \
STEVE_KEYHELPER_HUB_IDENTIFIER=org.example.steve.hub \
STEVE_KEYHELPER_IDENTIFIER=org.example.steve.keyhelper \
cargo build --manifest-path hub-rs/Cargo.toml -p steve-keyhelper --release
```

Only ASCII letters, digits, dots, dashes and underscores are accepted, preventing code-signing requirement injection. The binary still pins the parent's certificate to its own leaf certificate. An environment change after compilation cannot change the authorized caller.

Public defaults use `io.github.infranite.steve.*`. Existing installations must explicitly preserve their original build identities and keychain service; see `ops/KEYHELPER.md`. Both Hub and helper use the same compile-time `STEVE_KEYHELPER_SERVICE`. The release script exports the three build inputs, signs both binaries consistently, and includes the inputs in the helper source hash. It refuses replacement of an installed helper whose signing identifier differs. Actual credential migration and deployed signature changes were not performed.

## Idle statistics

The refresher performs a read-pool existence check before joining the writer queue. With no dirty rows or recent item decisions, it updates its overlap watermark and returns without taking the writer mutex or opening a write transaction. A positive hint is re-read in the actual refresh transaction. A concurrent commit after the read is handled by the next tick; the decision timestamp overlap remains intact. This change reduces empty write-lock acquisitions, not the two-second timer frequency, and makes no disk IOPS claim.
