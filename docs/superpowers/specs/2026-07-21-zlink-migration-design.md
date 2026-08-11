# Zlink Migration Design

## Objective

Determine whether Aileron should replace the synchronous `varlink` and
`varlink_generator` crates with `zlink`, and complete the replacement if it
passes the required compatibility and reliability gates.

The decision must be based on an implemented end-to-end migration, automated
tests, and interoperability checks. The migration is not accepted merely
because the workspace compiles.

## Decision Scope

This work replaces only Aileron's Varlink implementation. `zbus` remains the
D-Bus implementation for the desktop portal. The portal's public D-Bus API,
the runtime container protocol, model manifests, and inference semantics are
outside the migration scope except where their existing behavior must be
preserved.

The migration may use either a released `zlink` version or a pinned upstream
Git revision containing a small, upstreamable compatibility fix. A local fork
or permanently vendored dependency is not an acceptable outcome.

## Existing Constraints

Aileron exposes four interfaces from one Unix socket:

- `aileron.Inference`
- `aileron.Models`
- `aileron.Permissions`
- `aileron.Sessions`

The source `.varlink` files define the public wire contract. Existing clients
include the Aileron portal, manager UI, Rust components, the Rust `varlink`
CLI documented in the README, and systemd `varlinkctl`.

The current synchronous implementation requires a blocking server thread and
handler-level `tokio::runtime::Handle::block_on` calls. Active streams share
cloneable, locked connections, and cancellation closes the underlying socket.
These implementation details are not part of the public contract.

## Chosen Approach

Use `aileron-varlink` as a compatibility contract layer while replacing its
implementation with `zlink`.

The contract layer will provide:

- owned shared wire types;
- typed errors for each interface;
- ordinary async client proxies;
- explicit streaming proxies for methods that produce multiple replies;
- stable module boundaries or deliberate consumer updates that keep protocol
  details out of UI and portal code.

The existing `.varlink` files remain the protocol source of truth. The
contract crate will contain reviewed, handwritten bindings because
`zlink-codegen` cannot express the required streaming annotations and emits
borrowed outputs that are unsuitable for streams. Structural introspection
tests prevent the handwritten Rust contract from drifting from the IDLs.

A dual old/new server will not be retained. It would add socket ownership and
behavioral complexity without providing a shipped compatibility requirement.

## Server Architecture

The daemon will run a native async `zlink` server on the existing socket path.
The synchronous `varlink::listen`, its `spawn_blocking` wrapper, and all
handler-level runtime `block_on` bridges will be removed.

One service dispatcher will expose all four interfaces and standard
`org.varlink.service` introspection. Service metadata remains equivalent to
the current vendor, product, version, and URL values.

Short operations may be awaited directly. Long-running operations must not
block the single `zlink` service dispatcher. They will start work in Tokio
tasks and return channel-backed streams or await task results in a form that
allows the dispatcher to continue serving other connections. Existing shared
state synchronization remains authoritative.

## Client Architecture

`aileron-ipc` will expose async path-based `zlink` connections while retaining
the existing socket resolution rules:

1. `$AILERON_RUNTIME_DIR/aileron.socket`
2. `$XDG_RUNTIME_DIR/aileron.socket`
3. `/run/user/<uid>/aileron.socket`

The `unix:` address representation may remain for documentation and external
CLI use, but internal `zlink` clients connect with the filesystem path.

Each active streaming operation receives a dedicated connection. Ordinary
calls may also use short-lived connections unless a consumer has a clear need
to reuse one sequentially. No `Arc<RwLock<Connection>>` compatibility wrapper
will be introduced.

The GTK manager will use one controlled Tokio integration boundary rather
than running async calls through scattered synchronous Varlink APIs. GTK state
updates continue on the GLib main context.

## Streaming Semantics

Methods that currently emit multiple replies become explicit `zlink` streams:

- text response generation;
- guided response generation and tool continuation;
- transcription;
- image description and OCR where the current handler streams text.

Methods whose names begin with `Stream` or whose callers request `more=true`,
but which currently emit one terminal result, retain that wire behavior. The
server accepts `more=true`, emits one reply, and terminates it with
`continues=false` or no continuing flag as allowed by Varlink.

For true streams:

- intermediate successful replies set `continues=true`;
- the final successful reply terminates the stream;
- declared errors are typed stream items and terminate the stream;
- empty streams preserve the portal's current terminal event behavior;
- guided tool calls remain terminal and preserve snapshot suppression.

All streaming reply and error values are owned so they satisfy `zlink`'s
streaming deserialization requirements.

## Cancellation And Disconnection

Client cancellation aborts the local operation and drops its dedicated
connection. A connection with an abandoned in-flight call is never reused.

Server-side long-running work uses bounded channels. Producers check send
failures and receiver closure on every produced item and exit without further
application work when the receiver closes. Tests must cover disconnects before
the first reply and during a stream. In controlled tests, the producer task
must exit within two seconds of its next attempted send after disconnection.
An indefinitely retained inference or install operation after client
disconnection is an adoption blocker unless a small upstream `zlink` fix
resolves it.

The design does not add a new public Varlink cancellation method. After this
design was approved, `CancelActiveRequest` landed independently on `main`; the
migration preserves that existing wire contract when rebased rather than
introducing another cancellation method.

## Errors

Generated error display strings are not stable application interfaces.
Consumers will match typed interface error variants and fields instead of
parsing names such as `InstallFailed_Args`.

The following remain stable:

- declared Varlink error names and parameter fields;
- portal D-Bus error names;
- availability codes;
- user-facing error meaning.

Transport and JSON failures remain distinct from declared interface errors.

## Wire Compatibility

The following are hard compatibility requirements:

- all four interface names and method names;
- input and output JSON field names and types;
- declared error names and fields;
- NUL-terminated Varlink framing;
- `more` and `continues` behavior;
- standard `org.varlink.service.GetInfo`;
- interface description introspection;
- existing socket location and environment overrides.

The service-generated interface descriptions will be parsed and compared with
the checked-in IDLs structurally. Formatting and comments may differ; methods,
types, errors, names, and signatures may not.

## Test Strategy

### Contract Tests

- Preserve existing backward-compatible deserialization tests.
- Test every shared wire type needed by the four interfaces.
- Test typed declared errors without relying on display formatting.
- Test socket path resolution and stale socket cleanup.

### In-Process Wire Tests

Start the service on a temporary Unix socket and test:

- `GetInfo` and all four interface descriptions;
- at least one success and every declared error family;
- every ordinary method's request/reply serialization;
- every true streaming method's reply sequence and termination;
- single-result methods invoked with `more=true`;
- simultaneous requests from separate connections;
- client disconnect before a reply and during a stream;
- service startup, stale socket handling, and shutdown behavior.

Tests may use controlled test handlers where model containers or hardware are
not required, but they must exercise the real protocol contract and service
dispatch implementation.

### External Interoperability

Both clients are hard gates:

- systemd `varlinkctl`;
- the Rust `varlink` CLI currently documented by Aileron.

Each client must successfully exercise service information, interface
introspection where supported, an ordinary call, a declared error, and a
multi-reply stream with correct termination. Test tooling may be built or
installed under `/tmp/opencode` without modifying the user's global setup.

### Project Validation

Run formatting checks, Clippy with the repository's normal policy, all
workspace tests, and relevant portal/runtime integration tests supported by
the environment. Existing tests must be updated only when they assert an old
implementation detail rather than public behavior.

## Adoption Gates

Adopt `zlink` only when all of these are true:

1. `varlink` and `varlink_generator` are absent from normal and build
   dependencies.
2. The daemon and internal clients use native async `zlink` APIs.
3. Public Varlink and portal D-Bus behavior remains stable.
4. In-process wire and concurrency tests pass.
5. Cancellation passes the bounded-channel producer exit requirement and does
   not retain an inference or install task indefinitely.
6. Both external CLI interoperability suites pass.
7. Formatting, Clippy, and the full workspace test suite pass.
8. Any Git-pinned `zlink` fix is small, upstreamable, documented, and covered
   by a regression test.

If any hard gate cannot be met, the final decision is "do not adopt yet."
Migration-only code changes will not be retained as a partial production
switch. The final report will identify the failed gate, reproduction steps,
and the upstream capability or fix needed to reconsider the migration.

## Deliverables

- the complete retained migration, or a cleanly rejected migration;
- automated protocol and interoperability tests;
- updated dependency and developer documentation if adopted;
- a final go/no-go decision with commands run, results, and residual risks.
