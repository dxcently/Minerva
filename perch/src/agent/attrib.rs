//! Which chat a call came from, by the socket it arrived on (slice AG fills this).
//!
//! The call's [`crate::serve::Peer`] address, the `/proc/net/tcp` row whose local address it is, that row's inode, the live door whose
//! `/proc/<pid>/fd` holds `socket:[inode]` ([`crate::sessions::Registry::door_pids`]), and that door's chat id. Door pids only, with no
//! walk up through parents: a `bash` or `curl` child of a door is not a door.
