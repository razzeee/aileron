# Request cancellation and registration lifetime

Request epochs never wrap. `u64::MAX` permanently rejects new work for that
session. Closing a session advances its epoch; reopening retains that history
so an old token cannot become valid again. Epoch tombstones consume one map
entry per cancelled/closed session for the process lifetime.

Cancellation holds the epoch mutex while advancing the epoch and snapshotting
and marking active handles terminating. The async handler retires and waits on
that snapshot, rather than enumerating again after newer requests can capture
their epoch. When both locks are needed, epochs precede closed sessions.

Each active registration gets a process-lifetime, non-reusing atomic ID. RAII
cleanup removes only that ID, even if a newer registration uses the same handle.
Allocator exhaustion panics before insertion rather than reusing an ID.

The API remains cooperative. A request registered after the cancellation
snapshot relies on its guard or polling watcher; termination latency is not
bounded. Shared containers can affect other sessions when terminated. The
session-close path assumes callers do not concurrently reopen the same ID.

Regression coverage exercises the real mutexes, RAII cleanup across threads,
snapshot ordering, late registration watchers, close/reopen and exhaustion:

```sh
cargo test --locked -p aileron-daemon --all-targets
cargo clippy --locked -p aileron-daemon --all-targets -- -D warnings
```
