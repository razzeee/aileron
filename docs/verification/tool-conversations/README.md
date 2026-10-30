# Tool-conversation Verus pilot

`crates/aileron-tool-conversation-core/src/lib.rs` is both the verified source and
the implementation called by `ToolConversation`. Cargo uses a pinned Verus macro
to erase proof syntax; there is no handwritten Rust mirror or generated-code
copy. The daemon has one new local dependency. This is the project's only Verus
pilot; expansion depends on maintenance experience with real lifecycle changes.

## Proved guarantees

The exported contracts on the executable core establish:

- `start` allocates a strictly greater serial or returns exhaustion. It never
  wraps. Exhausted starts clear the old state and remain exhausted.
- `cancel` clears pending IDs and enters Idle without reducing the serial.
  Remember/complete cannot accept in Idle. Any subsequent successful start has a
  greater serial, so cancellation cannot revalidate an earlier turn.
- `remember` accepts only the current Running turn, then stores a unique ID
  batch and enters Waiting. Rejected calls leave the core unchanged.
- A successful continuation requires Waiting and exact ID-set equality with
  uniqueness on **both** sides. It consumes the batch and allocates a strictly
  greater serial. Failed continuations leave the core unchanged, including at
  exhaustion. ID equality is string equality, not a hash or numeric encoding.
- `complete` accepts exactly a non-Idle current turn. Stale completions leave
  the core unchanged. Completing Running clears it; completing Waiting preserves
  the batch (runtimes may send `tool_calls` then a separate `done` event).

Together these contracts prevent earlier turn callbacks from mutating a newer
turn in the same core. All IDs ever issued by that instance are at most its
monotone serial. No assumption of infinitely fresh `u64` values is made.
At `u64::MAX`, the user must create a new session; cancellation still works.

The exact-result contract is a **soundness** guarantee: acceptance implies both
membership directions and no duplicates. It does not formally promise acceptance
of every valid permutation. An independent set-oracle test covers 1,600 small
input pairs. Runtime checks retain the existing nonempty/32-call batch bound.

## Integration and trusted boundaries

The ordinary Rust adapter owns JSON, payload/history cloning, size/round limits,
historical-ID reuse checks and JSON parsing. It validates payloads before
committing a continuation, then clears the adapter's pending payload. Invalid
JSON/results retain the pending batch. The core owns the authoritative pending
IDs; the adapter supplies verbatim cloned strings, not hashes.

The proof does **not** verify the adapter-to-core payload association, serde,
resource limits, historical-ID checks, allocator failure, async scheduling,
runtime protocol compliance, streaming event delivery, filesystem/process I/O,
request cancellation, permissions, or whole-application correctness. The caller
must retain the same session's mutex/core for callbacks; tokens are not globally
unique across independent sessions and are not authentication capabilities.
Generation's `Arc<Mutex<ToolConversation>>` supplies that ownership/serialization.
Session-ending/cancellation callers retain their existing `cancel()` interface.

Continuation now consumes its pending batch: retrying after a downstream runtime
failure requires a new start rather than silently replaying the same turn.
Repeated tool-call batches for one turn are rejected. Adapter regression tests
exercise these transitions, stale callbacks, malformed results, and split done.

Trusted verification stack: Verus/vstd specifications, SMT solver, Rust compiler,
standard library and proof-erasure macro. No custom `assume`, `external_body`,
or assumed external specifications were added. Verus is pinned to
`0.2026.10.04.426d8b0`; Cargo's erasure-only macro is independently pinned to
`0.0.0-2026-10-18-0147` in Cargo.lock. Normal builds require neither Verus nor Z3.

## Reproduce

From the repository root, with the pinned Verus distribution and its required
Rust toolchain installed:

```sh
python3 crates/aileron-tool-conversation-core/verify.py /path/to/verus /tmp/opencode/tool-conversations-new-run
cargo test --locked -p aileron-tool-conversation-core -p aileron-daemon --lib
cargo clippy --locked -p aileron-tool-conversation-core -p aileron-daemon --lib --tests -- -D warnings
```

The verification runner requires a **new** evidence directory, records the exact
source/hash, command, output and duration, verifies the production file, then
requires proof failure for five mutations: stale completion acceptance, cancel
without invalidation, wrapping ID allocation, duplicate acceptance, and unknown
ID acceptance. Parse errors do not count as successful negative controls.

## CI and maintenance

`.github/workflows/tool-conversation-verification.yml` verifies the core and all
five negative controls on changes to the core, daemon, dependency manifests,
toolchain, pilot docs or workflow. It also runs the core and daemon library
tests and lints. Manual runs are available through `workflow_dispatch`.

CI installs Rust 1.98.1 for the verifier and checks the Linux release archive
against SHA-256
`624018662868563a7bec0dd07be5e4dd0c3d61124b4b55105765884bf2d4d1a0`.
Ordinary Rust tests use stable. The release URL and checksum are pinned in the
workflow; update them together with the runner's version check when upgrading.
The erasure macro has its own Cargo pin and must be checked for compatibility.

Proof inputs, hashes, timings, diagnostics and mutation results are uploaded as
CI artifacts for 30 days, including failed proof runs. Initial experiment logs
and repeated source snapshots remain available in Git history before the pilot
cleanup; new evidence belongs in artifacts, not the source tree.

On each lifecycle change, review the contracts as well as the implementation:
a passing proof cannot detect an incorrectly weakened contract. Review the JSON
adapter and caller mutex ownership separately. Record proof repair effort and
any bugs uniquely caught by verification in the PR description. Evaluate those
results before adopting Verus elsewhere; initial implementation time is not a
measurement of long-term maintenance cost.
