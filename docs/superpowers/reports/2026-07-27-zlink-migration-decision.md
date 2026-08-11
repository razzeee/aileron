# Zlink Migration Decision

## Decision

Adopt `zlink` 0.7.0 for Aileron's Varlink implementation.

All hard adoption gates in the [migration design](../specs/2026-07-21-zlink-migration-design.md)
pass. The `zbus` desktop portal implementation and public D-Bus API remain
unchanged.

## Adoption Gates

1. `varlink` and `varlink_generator` are absent from normal and build dependencies.
2. The daemon, portal, and manager use native async `zlink` connections and proxies.
3. Handwritten owned contracts are structurally checked against all four IDLs.
4. The production service exposes all four interfaces and standard service introspection.
5. True streams, single-result `more=true` calls, typed errors, concurrency, and socket lifecycle pass in-process wire tests.
6. Bounded producers stop after disconnect before the first reply and during a stream; operation-local cancellation terminates active inference work.
7. Both systemd `varlinkctl` and the Rust `varlink` CLI pass production-service and controlled real-interface streaming suites.
8. Formatting, warning-denied Clippy, and all workspace tests pass.

No Git-pinned fix or local fork is used.

## Validation

Commands run on 2026-07-27:

```sh
cargo fmt --all -- --check
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
AILERON_REQUIRE_EXTERNAL_VARLINK_CLIENTS=1 \
    cargo test -p aileron-varlink --test external_interop -- --nocapture
AILERON_REQUIRE_EXTERNAL_VARLINK_CLIENTS=1 \
    cargo test -p aileron-daemon --test external_interop -- --nocapture
```

Results:

- Workspace tests passed; two existing environment-dependent daemon tests remained ignored.
- The exhaustive hardware-free wire suite passed for every method, declared error, and stream shape.
- Both installed external clients passed metadata, introspection, ordinary-call, typed-error, and multi-reply stream checks.
- Clippy completed with `-D warnings` and formatting checks passed.

## Residual Risks

- Successful production inference still depends on an installed model runtime. External multi-reply coverage therefore uses a controlled service with the exact `aileron.Inference` contract; the production dispatcher is tested separately with both external clients.
- The ignored container-rootfs roundtrip and real OCI-layout tests still require their documented external fixtures.
- `zlink` does not expose peer-disconnect callbacks. Aileron handles cancellation through bounded stream closure and operation-local watchers, covered by two-second regression tests.
- The migration also preserves `CancelActiveRequest`, which landed independently on `main` after the design was written.
