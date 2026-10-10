# Twelve-Step Optimization Plan

Implemented without changing search ranking, visible animation cadence, window
ordering rules, restoration policy, snapshot retention, or release compiler flags.

1. **Reuse tmux identities and per-poll process reads.** Validate client inode,
   start time and executable before reusing its socket. Query live titles every
   poll; resolve each shared pane once. Process scratch data expires every poll.
2. **Virtualize window rows and application tiles.** Allocate the entire scroll
   extent but render only visible rows plus one overscan row. Retain global item
   indexes, stable widget IDs, selection scrolling and cross-pane navigation.
3. **Cache default window ordering.** Reuse the sorted row model across frames.
   Invalidate on real identity/grouping/sort changes and activation changes;
   patch spinner frames in place without sorting.
4. **Separate transient title frames from metadata changes.** Spinner/attention
   frames reuse verified window metadata for at most 500 ms. Audio attribution
   still refreshes for ownership changes or explicit stream-title matches.
5. **Prepare search documents incrementally.** Prepare only dirty/new windows;
   reuse unchanged document allocations. Empty-query startup does not eagerly
   prepare documents. Removed windows are pruned from the dirty set.
6. **Send bounded incremental tracker updates.** GetWindowChanges returns the
   latest changed records and removal IDs in one RPC. A daemon restart or journal
   overflow requests an authoritative resynchronization. Legacy daemons retain
   the GetStatus/GetWindows fallback. An inactive GUI backlog exceeding 4096
   events is replaced by one authoritative snapshot, not historical removals.
7. **Separate visual updates from durable database changes.** Ignore spinner and
   blinking attention frames for persistence. Patch only changed/deleted records
   transactionally; retain newer dirty revisions arriving during I/O. Restore
   metadata reuse also expires at 500 ms, independently of title frames.
8. **Refresh history by generation.** Reuse its tracker connection and fetch
   history/snapshots only when their generation or daemon run changes. Record the
   generation read before fetching, so concurrent changes trigger another read.
   Both history and session tabs continue refreshing while open.
9. **Cache desktop-entry parsing.** Key parsed data by inode, size, nanosecond
   modification/change times and locale. Enumerate paths every scan, reevaluate
   TryExec and desktop visibility, and resolve each icon name once per scan.
   Negative icon lookups never persist between scans.
10. **Map highlight offsets once.** Build one Unicode byte-offset table per
    rendered search string instead of repeatedly scanning prefixes. Preserve all
    occurrences, exact-over-typo precedence and focus on the winning match.
11. **Write settings off the UI thread.** A named worker coalesces the latest
    settings, pins and size values in a three-path queue. It keeps the existing
    atomic-write/fsync behavior; normal GUI exit drains queued writes after the
    event loop has stopped. Worker and I/O failures remain visible as errors.
12. **Collect subprocess output with readiness notifications.** On Linux, poll
    stdout/stderr and pidfd in the calling worker instead of creating two reader
    threads and polling child exit every 10 ms. Retain a bounded compatibility
    fallback, four-MiB captures per pipe, deadline checks and private-group cleanup
    before reaping. Application launches still use independent scopes.

## Verification

The scoped behavior inventory is [optimization-contracts.json](../tests/optimization-contracts.json).
Native regression tests check work counts, layout/IDs, Unicode mapping, journal
overflow/reconnect, in-flight writes, file freshness and bounded worker queues.
These prove work reduction and preservation of the specified contracts, not a
particular live desktop CPU percentage or FPS improvement.

```sh
cargo fmt --check
cargo check --offline
cargo test --offline --release
cargo test --offline --release --lib private_server_live_titles_preserve_spinners_and_escape_record_controls -- --ignored
dbus-run-session -- env APPLICATIONLAUNCHER_TEST_PRIVATE_BUS=1 cargo test --offline --release --lib private_bus_incremental_feed_and_legacy_fallback_preserve_end_state -- --ignored
python -m unittest discover -s tests -p test_firefox_bridge_install.py
cargo build --offline --release --bins
```

The opt-in tests above own an isolated tmux server and a private D-Bus session;
they do not restore, close or inject input into the user's windows. Live GPU,
desktop activation, audio capture and terminal-maintenance tests remain separate
environment-dependent checks. Do not run all ignored tests against a live desktop.
