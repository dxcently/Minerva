//! Which chat a call came from, by the socket it arrived on (slice AG).
//!
//! The call's [`crate::serve::Peer`] address is the *client's* end of its connection: the kernel's word, and the one thing a caller
//! cannot choose. Three steps, none of which reads anything the caller sent:
//!
//! ```text
//!   1  the kernel's socket table   the socket whose local address is the peer and whose remote address is this perch   ->  its inode
//!   2  /proc/<pid>/fd              the live door whose descriptors hold socket:[inode]                                 ->  that door's pid
//!   3  Registry::id_of_pid         the pid, asked again, is still a live door                                         ->  the chat id
//! ```
//!
//! **Step 1 is asked two ways.** The primary is `sock_diag`, the netlink query `ss` uses, with the whole four-tuple: the kernel looks
//! the one socket up in its hash and answers with its inode, so the cost does not grow with the number of sockets on the box. The
//! fallback is `/proc/net/tcp` (and `tcp6`), the row whose local address is the peer and whose remote address is this perch, which is
//! linear in the table (about 10 microseconds a row: a box with a thousand connections spends 13 ms) and is read only when `sock_diag`
//! gave no answer (a kernel without `tcp_diag`, or a connection that is already gone). The two agree, and a test says so.
//!
//! **Door pids only, with no walk up through parents.** A `bash` or `curl` the agent ran has its own socket and its own descriptor
//! table, so the socket is in no door's: it is refused, as are a subagent's children, the TUI and a workflow run. A caller that maps to
//! no door is `None`, and the gate says 403. This is a guard rail and not a boundary: a process of the same uid can take a door's
//! descriptor (`pidfd_getfd`) or read the master token, and the attribution says only which door's socket the bytes arrived on.
//!
//! **Fail closed.** No answer, two table rows with different inodes (the row for this connection is unique, so two is not a thing to
//! guess between), a socket with no inode (a `TIME_WAIT` shell), a door whose descriptors cannot be read (another user's process, one
//! that has gone): each is `None`. The proc table is read twice before "no row" is believed, because the kernel's listing of a hash
//! that is changing can miss a row once.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::ffi::OsStrExt as _;
use std::sync::Mutex;

use crate::sessions::Registry;

const TCP: &str = "/proc/net/tcp";
const TCP6: &str = "/proc/net/tcp6";

/// The chat id of the live door whose socket carried a call from `peer` to the perch listening on `bound`, if there is one.
pub fn chat_of(registry: &Registry, peer: SocketAddr, bound: SocketAddr) -> Option<String> {
    chat_of_hinted(registry, &Hints::default(), peer, bound)
}

/// [`chat_of`], with a memory of where sockets were found, which is what the gate uses.
///
/// The hints only make a repeat cheap: an agent's calls come one after another from one door and often down one kept-alive
/// connection, so the socket whose inode the kernel names now was found in some descriptor a moment ago. A hint is *asked, not
/// believed*: it is one `readlink` of that descriptor, and the answer must be this very `socket:[inode]`, with the inode the kernel
/// named for this connection just now. So a hint can never make a connection out to be another one; it only skips the walk over every
/// door's descriptors. A hint that fails is dropped and the walk is made. The last step, [`Registry::id_of_pid`], decides whether the
/// pid is a live door, hint or no hint.
pub fn chat_of_hinted(registry: &Registry, hints: &Hints, peer: SocketAddr, bound: SocketAddr) -> Option<String> {
    let inode = socket_inode(peer, bound)?;
    let want = format!("socket:[{inode}]");
    let pid = match hints.get(inode) {
        Some((pid, fd)) if holds_at(pid, fd, want.as_bytes()) => pid,
        stale => {
            if stale.is_some() {
                hints.forget(inode);
            }
            let (pid, fd) = registry.door_pids().into_iter().find_map(|(_, pid)| find_fd(pid, want.as_bytes()).map(|fd| (pid, fd)))?;
            hints.remember(inode, pid, fd);
            pid
        }
    };
    registry.id_of_pid(pid)
}

/// Where a socket was last found: its inode to the pid and descriptor number that held it. At most [`Hints::CAP`] of them; a full
/// table is emptied, which costs the next lookups one walk each.
#[derive(Default)]
pub struct Hints {
    found: Mutex<HashMap<u64, (u32, u32)>>,
}

impl Hints {
    const CAP: usize = 256;

    fn get(&self, inode: u64) -> Option<(u32, u32)> {
        self.found.lock().unwrap().get(&inode).copied()
    }

    fn forget(&self, inode: u64) {
        self.found.lock().unwrap().remove(&inode);
    }

    fn remember(&self, inode: u64, pid: u32, fd: u32) {
        let mut found = self.found.lock().unwrap();
        if found.len() >= Self::CAP {
            found.clear();
        }
        found.insert(inode, (pid, fd));
    }
}

/// One way the connection can be written down in the kernel's tables: the client's end (`local`) and the perch's (`remote`), in the
/// family the table is for. A v4 connection is in the v4 table, and also in the v6 one as v4-mapped addresses when the client's
/// socket was an `AF_INET6` one.
#[derive(Clone, Copy, Debug)]
struct Probe {
    table: &'static str,
    local: SocketAddr,
    remote: SocketAddr,
}

/// A peer and a bound address of different families cannot be one connection.
fn probes(peer: SocketAddr, bound: SocketAddr) -> Option<Vec<Probe>> {
    let mapped = |addr: SocketAddr, ip: Ipv4Addr| SocketAddr::new(IpAddr::V6(ip.to_ipv6_mapped()), addr.port());
    match (peer.ip().to_canonical(), bound.ip().to_canonical()) {
        (IpAddr::V4(p), IpAddr::V4(b)) => Some(vec![
            Probe { table: TCP, local: SocketAddr::new(IpAddr::V4(p), peer.port()), remote: SocketAddr::new(IpAddr::V4(b), bound.port()) },
            Probe { table: TCP6, local: mapped(peer, p), remote: mapped(bound, b) },
        ]),
        (IpAddr::V6(p), IpAddr::V6(b)) => {
            Some(vec![Probe { table: TCP6, local: SocketAddr::new(IpAddr::V6(p), peer.port()), remote: SocketAddr::new(IpAddr::V6(b), bound.port()) }])
        }
        _ => None,
    }
}

/// The inode of the client's socket for the connection `peer` -> `bound`, or `None`: `sock_diag` first, then the proc tables.
pub fn socket_inode(peer: SocketAddr, bound: SocketAddr) -> Option<u64> {
    let probes = probes(peer, bound)?;
    probes.iter().find_map(|probe| diag::inode_of(probe.local, probe.remote)).or_else(|| inode_in_proc(&probes))
}

/// [`socket_inode`] by `sock_diag` alone. For a test to say the two ways agree.
pub fn socket_inode_by_diag(peer: SocketAddr, bound: SocketAddr) -> Option<u64> {
    probes(peer, bound)?.iter().find_map(|probe| diag::inode_of(probe.local, probe.remote))
}

/// [`socket_inode`] by the proc tables alone.
pub fn socket_inode_by_proc(peer: SocketAddr, bound: SocketAddr) -> Option<u64> {
    inode_in_proc(&probes(peer, bound)?)
}

fn inode_in_proc(probes: &[Probe]) -> Option<u64> {
    for _ in 0..2 {
        let mut found: Vec<u64> = Vec::new();
        for probe in probes {
            if let Ok(text) = std::fs::read_to_string(probe.table) {
                found.extend(inodes_in(&text, &hex(probe.local), &hex(probe.remote)));
            }
        }
        found.sort_unstable();
        found.dedup();
        match found[..] {
            [inode] => return Some(inode),
            [] => continue,
            _ => return None,
        }
    }
    None
}

fn hex(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(addr) => v4_hex(*addr.ip(), addr.port()),
        SocketAddr::V6(addr) => v6_hex(*addr.ip(), addr.port()),
    }
}

/// The kernel prints an address as `%08X` of each 32-bit word of the raw (network order) bytes read in host order, and a port as `%04X`:
/// `127.0.0.1:8080` is `0100007F:1F90` on a little-endian machine, which is what `from_ne_bytes` is.
fn v4_hex(ip: Ipv4Addr, port: u16) -> String {
    format!("{:08X}:{:04X}", u32::from_ne_bytes(ip.octets()), port)
}

/// The same for v6: four such words, then the port. `::1:8080` is `00000000000000000000000001000000:1F90`.
fn v6_hex(ip: Ipv6Addr, port: u16) -> String {
    let mut out = String::with_capacity(37);
    let octets = ip.octets();
    for first in [0, 4, 8, 12] {
        let _ = write!(out, "{:08X}", u32::from_ne_bytes([octets[first], octets[first + 1], octets[first + 2], octets[first + 3]]));
    }
    let _ = write!(out, ":{port:04X}");
    out
}

/// The inodes of the rows of one table (`/proc/net/tcp`'s text) whose local and remote addresses are these. Columns: `sl local rem st
/// tx:rx tr:tm retrnsmt uid timeout inode ...`, so the inode is the tenth. A row with inode 0 is a socket with no file behind it (a
/// `TIME_WAIT` shell): it cannot be anyone's descriptor and is not an answer.
fn inodes_in(table: &str, local: &str, remote: &str) -> Vec<u64> {
    table
        .lines()
        .filter_map(|row| {
            let mut field = row.split_ascii_whitespace();
            let (_sl, l, r) = (field.next()?, field.next()?, field.next()?);
            if !l.eq_ignore_ascii_case(local) || !r.eq_ignore_ascii_case(remote) {
                return None;
            }
            let inode: u64 = field.nth(6)?.parse().ok()?;
            (inode != 0).then_some(inode)
        })
        .collect()
}

/// The descriptor number under which process `pid` holds `want` (`socket:[inode]`), if it does. A process whose descriptors cannot be
/// listed (gone since the snapshot, or not ours to look at) holds nothing.
///
/// This is the hot loop (every door's descriptors, once per lookup that has no hint), so it asks the kernel for each link relative to
/// the directory it has open (`readlinkat`) into a buffer on the stack, and compares bytes: no path is walked from `/proc` again for
/// each descriptor and nothing is allocated. A link longer than the buffer cannot be `socket:[<u64>]` and is skipped.
fn find_fd(pid: u32, want: &[u8]) -> Option<u32> {
    use std::ffi::{CStr, CString};
    let dir_path = CString::new(format!("/proc/{pid}/fd")).ok()?;
    // SAFETY: `opendir` takes a NUL-terminated path and returns an owned stream or null; it is closed on every path out below.
    let dir = unsafe { libc::opendir(dir_path.as_ptr()) };
    if dir.is_null() {
        return None;
    }
    // SAFETY: `dir` is a live stream; its descriptor is valid until `closedir`.
    let dirfd = unsafe { libc::dirfd(dir) };
    let mut link = [0u8; 64];
    let mut found = None;
    loop {
        // SAFETY: `readdir` on a live stream that only this function uses; the entry is read before the next call.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is a NUL-terminated name inside the entry just returned.
        let name = unsafe { (*entry).d_name.as_ptr() };
        // SAFETY: `name` is NUL-terminated, `link` is a live buffer of the length passed, `dirfd` is the open directory.
        let len = unsafe { libc::readlinkat(dirfd, name, link.as_mut_ptr().cast(), link.len()) };
        if len >= 0 && link[..len as usize] == *want {
            // SAFETY: `name` is the NUL-terminated name read above and the entry has not been advanced past.
            found = unsafe { CStr::from_ptr(name) }.to_str().ok().and_then(|name| name.parse().ok());
            break;
        }
    }
    // SAFETY: closing the stream opened above, once.
    unsafe { libc::closedir(dir) };
    found
}

/// Whether descriptor `fd` of process `pid` is `want` right now: the one `readlink` that a hint costs.
fn holds_at(pid: u32, fd: u32, want: &[u8]) -> bool {
    std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).is_ok_and(|link| link.as_os_str().as_bytes() == want)
}

/// `sock_diag`: one netlink request for one socket by its four-tuple, one reply. Everything it needs is below; there is no
/// dependency, only `socket`, `sendto` and `recv`.
///
/// The request is a `SOCK_DIAG_BY_FAMILY` with `inet_diag_req_v2` for TCP and the four-tuple in `inet_diag_sockid`, and no dump flag,
/// which is the kernel's exact lookup (`inet_diag_find_one_icsk`: the hash, not a walk). The reply is the socket's `inet_diag_msg`, whose
/// last field is the inode. The reply is **checked, not trusted**: it must be a diag message (not an error), for the same family, with
/// the same ports and addresses as were asked for (an exact lookup that finds nothing may fall back to a *listener* on the local port,
/// which has other addresses and is no answer), and a non-zero inode.
mod diag {
    use std::net::{IpAddr, SocketAddr};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    const NETLINK_SOCK_DIAG: libc::c_int = 4;
    const SOCK_DIAG_BY_FAMILY: u16 = 20;
    const NLM_F_REQUEST: u16 = 1;
    /// `INET_DIAG_NOCOOKIE`: the cookie that says "do not check one".
    const NOCOOKIE: u32 = !0;
    const NLMSG_HDR: usize = 16;
    /// `inet_diag_req_v2`: family, protocol, ext, pad, states (4), then `inet_diag_sockid` (48).
    const REQ_LEN: usize = 4 + 4 + SOCKID_LEN;
    /// `inet_diag_sockid`: sport (2), dport (2), src (16), dst (16), interface (4), cookie (8).
    const SOCKID_LEN: usize = 2 + 2 + 16 + 16 + 4 + 8;
    /// `inet_diag_msg`: family, state, timer, retrans (4), the sockid (48), expires, rqueue, wqueue, uid, inode (5 x 4).
    const MSG_LEN: usize = 4 + SOCKID_LEN + 5 * 4;
    const INODE_AT: usize = NLMSG_HDR + MSG_LEN - 4;

    /// The sockid for a socket that is `local` -> `remote`, as the kernel wants it: ports and addresses in network order.
    fn sockid(local: SocketAddr, remote: SocketAddr) -> [u8; SOCKID_LEN] {
        let mut id = [0u8; SOCKID_LEN];
        id[0..2].copy_from_slice(&local.port().to_be_bytes());
        id[2..4].copy_from_slice(&remote.port().to_be_bytes());
        id[4..20].copy_from_slice(&address(local.ip()));
        id[20..36].copy_from_slice(&address(remote.ip()));
        // interface 0 (any), then the cookie
        id[40..44].copy_from_slice(&NOCOOKIE.to_ne_bytes());
        id[44..48].copy_from_slice(&NOCOOKIE.to_ne_bytes());
        id
    }

    /// A v4 address fills the first word of the 16 bytes, a v6 address all of them.
    fn address(ip: IpAddr) -> [u8; 16] {
        let mut out = [0u8; 16];
        match ip {
            IpAddr::V4(ip) => out[..4].copy_from_slice(&ip.octets()),
            IpAddr::V6(ip) => out.copy_from_slice(&ip.octets()),
        }
        out
    }

    fn request(local: SocketAddr, remote: SocketAddr) -> [u8; NLMSG_HDR + REQ_LEN] {
        let mut msg = [0u8; NLMSG_HDR + REQ_LEN];
        msg[0..4].copy_from_slice(&((NLMSG_HDR + REQ_LEN) as u32).to_ne_bytes());
        msg[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        msg[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());
        msg[8..12].copy_from_slice(&1u32.to_ne_bytes());
        // pid 0: the kernel addresses the reply to the socket it came from
        let family = if local.is_ipv4() { libc::AF_INET } else { libc::AF_INET6 };
        let body = &mut msg[NLMSG_HDR..];
        body[0] = family as u8;
        body[1] = libc::IPPROTO_TCP as u8;
        // ext 0, pad 0; every state: an exact lookup ignores it, and a socket in any state is the connection
        body[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
        body[8..].copy_from_slice(&sockid(local, remote));
        msg
    }

    /// The inode of the socket `local` -> `remote`, when the kernel names exactly that socket. `None` for everything else: no such
    /// socket, no `sock_diag` for TCP in this kernel, a reply that is not for this tuple, a socket with no inode.
    pub fn inode_of(local: SocketAddr, remote: SocketAddr) -> Option<u64> {
        // SAFETY: plain syscalls on a descriptor this function owns; every pointer is to a live local of the length passed.
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, NETLINK_SOCK_DIAG) };
        if fd < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // A kernel that never answers must not hold a worker: the lookup is local and answers in microseconds or not at all.
        let timeout = libc::timeval { tv_sec: 1, tv_usec: 0 };
        let set = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const timeout).cast(),
                size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if set != 0 {
            return None;
        }
        // `sockaddr_nl`: family (2), pad (2), pid (4), groups (4). To the kernel: pid 0, no groups.
        let mut to = [0u8; 12];
        to[0..2].copy_from_slice(&(libc::AF_NETLINK as u16).to_ne_bytes());
        let ask = request(local, remote);
        let sent = unsafe { libc::sendto(fd.as_raw_fd(), ask.as_ptr().cast(), ask.len(), 0, to.as_ptr().cast(), to.len() as libc::socklen_t) };
        if sent != ask.len() as isize {
            return None;
        }
        let mut reply = [0u8; 1024];
        let got = unsafe { libc::recv(fd.as_raw_fd(), reply.as_mut_ptr().cast(), reply.len(), 0) };
        if got < 0 {
            return None;
        }
        inode_in(&reply[..got as usize], &ask[NLMSG_HDR..])
    }

    /// The inode in a reply to `request` (the part after the netlink header), if it is a diag message for exactly that socket.
    fn inode_in(reply: &[u8], request: &[u8]) -> Option<u64> {
        if reply.len() < NLMSG_HDR + MSG_LEN {
            return None;
        }
        let word = |at: usize| u32::from_ne_bytes([reply[at], reply[at + 1], reply[at + 2], reply[at + 3]]);
        // A netlink header: length, type, flags, seq, pid. An error reply is type NLMSG_ERROR (2) and is shorter than this anyway.
        let kind = u16::from_ne_bytes([reply[4], reply[5]]);
        if kind != SOCK_DIAG_BY_FAMILY || (word(0) as usize) < NLMSG_HDR + MSG_LEN {
            return None;
        }
        let msg = &reply[NLMSG_HDR..NLMSG_HDR + MSG_LEN];
        // Same family, and the same ports and addresses (sport, dport, src, dst: the first 36 bytes of the sockid): an exact lookup that
        // found no established socket may have answered with a listener, whose remote address is nothing.
        let (asked_family, asked_id) = (request[0], &request[8..8 + 36]);
        if msg[0] != asked_family || &msg[4..4 + 36] != asked_id {
            return None;
        }
        let inode = u64::from(word(INODE_AT));
        (inode != 0).then_some(inode)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn v4(text: &str) -> SocketAddr {
            text.parse().unwrap()
        }

        /// A reply as the kernel writes it: the header, `inet_diag_msg` for the sockid in `request`, and the inode last.
        fn reply_for(request: &[u8; NLMSG_HDR + REQ_LEN], kind: u16, inode: u32) -> Vec<u8> {
            let mut out = vec![0u8; NLMSG_HDR + MSG_LEN];
            out[0..4].copy_from_slice(&((NLMSG_HDR + MSG_LEN) as u32).to_ne_bytes());
            out[4..6].copy_from_slice(&kind.to_ne_bytes());
            out[NLMSG_HDR] = request[NLMSG_HDR];
            out[NLMSG_HDR + 1] = 1; // established
            out[NLMSG_HDR + 4..NLMSG_HDR + 4 + SOCKID_LEN].copy_from_slice(&request[NLMSG_HDR + 8..]);
            out[INODE_AT..INODE_AT + 4].copy_from_slice(&inode.to_ne_bytes());
            out
        }

        #[test]
        fn the_request_is_the_exact_lookup_of_one_tuple() {
            let ask = request(v4("127.0.0.1:49999"), v4("127.0.0.1:8080"));
            assert_eq!(ask.len(), 72);
            assert_eq!(u32::from_ne_bytes(ask[0..4].try_into().unwrap()), 72, "the length is in the header");
            assert_eq!(u16::from_ne_bytes(ask[4..6].try_into().unwrap()), SOCK_DIAG_BY_FAMILY);
            assert_eq!(u16::from_ne_bytes(ask[6..8].try_into().unwrap()), NLM_F_REQUEST, "no dump flag: one socket, not a walk");
            assert_eq!(ask[NLMSG_HDR], libc::AF_INET as u8);
            assert_eq!(ask[NLMSG_HDR + 1], libc::IPPROTO_TCP as u8);
            let id = &ask[NLMSG_HDR + 8..];
            assert_eq!(&id[0..4], &[0xC3, 0x4F, 0x1F, 0x90], "ports in network order: 49999, 8080");
            assert_eq!(&id[4..8], &[127, 0, 0, 1]);
            assert_eq!(&id[8..20], &[0u8; 12], "a v4 address fills one word");
            assert_eq!(&id[20..24], &[127, 0, 0, 1]);
            assert_eq!(&id[40..48], &[0xFF; 8], "no cookie to check");
        }

        #[test]
        fn a_reply_is_believed_only_for_the_tuple_that_was_asked() {
            let ask = request(v4("127.0.0.1:49999"), v4("127.0.0.1:8080"));
            let good = reply_for(&ask, SOCK_DIAG_BY_FAMILY, 4242);
            assert_eq!(inode_in(&good, &ask[NLMSG_HDR..]), Some(4242));
            assert_eq!(inode_in(&reply_for(&ask, SOCK_DIAG_BY_FAMILY, 0), &ask[NLMSG_HDR..]), None, "a time-wait shell has no inode");
            assert_eq!(inode_in(&reply_for(&ask, 2, 4242), &ask[NLMSG_HDR..]), None, "an error reply is not an answer");
            assert_eq!(inode_in(&good[..good.len() - 1], &ask[NLMSG_HDR..]), None, "a short reply is not an answer");
            assert_eq!(inode_in(&[], &ask[NLMSG_HDR..]), None);
            // The same reply for another tuple: a different port, a different remote, a different family.
            for other in [request(v4("127.0.0.1:50000"), v4("127.0.0.1:8080")), request(v4("127.0.0.1:49999"), v4("127.0.0.1:8081")), request(v4("127.0.0.2:49999"), v4("127.0.0.1:8080"))] {
                assert_eq!(inode_in(&good, &other[NLMSG_HDR..]), None);
            }
            let six = request("[::1]:49999".parse().unwrap(), "[::1]:8080".parse().unwrap());
            assert_eq!(inode_in(&good, &six[NLMSG_HDR..]), None, "the family is checked");
            // A listener answering for the local port has no remote address: the remote half of the id is zeros, not the perch's.
            let mut listener = good.clone();
            listener[NLMSG_HDR + 4 + 20..NLMSG_HDR + 4 + 36].fill(0);
            assert_eq!(inode_in(&listener, &ask[NLMSG_HDR..]), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows as the kernel writes them: the header, a listener, the client's end of 49999 -> 8080, the server's end of the same
    /// connection (the addresses the other way round), and a `TIME_WAIT` shell with no inode.
    const V4: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11111 1 ffff9d1a8b3b0000 100 0 0 10 0
   1: 0100007F:C34F 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 22222 1 ffff9d1a8b3b0740 20 4 30 10 -1
   2: 0100007F:1F90 0100007F:C34F 01 00000000:00000000 00:00000000 00000000  1000        0 33333 1 ffff9d1a8b3b0e80 20 4 30 10 -1
   3: 0100007F:C350 0100007F:1F90 06 00000000:00000000 03:000009E2 00000000     0        0 0 3 ffff9d1a8b3b1100
";

    #[test]
    fn the_client_end_of_one_connection_is_the_row_whose_local_address_is_the_peer() {
        assert_eq!(inodes_in(V4, "0100007F:C34F", "0100007F:1F90"), [22222], "the client's socket, not the server's (33333)");
        assert_eq!(inodes_in(V4, "0100007F:1F90", "0100007F:C34F"), [33333], "the same row from the other side is the perch's own socket");
        assert!(inodes_in(V4, "0100007F:C350", "0100007F:1F90").is_empty(), "a shell with inode 0 is not a socket anyone holds");
        assert!(inodes_in(V4, "0100007F:C351", "0100007F:1F90").is_empty(), "another port");
        assert!(inodes_in(V4, "0200007F:C34F", "0100007F:1F90").is_empty(), "another address");
        assert!(inodes_in(V4, "0100007F:C34F", "0100007F:1F91").is_empty(), "another bound port");
        assert_eq!(inodes_in(&V4.to_ascii_lowercase(), "0100007F:C34F", "0100007F:1F90"), [22222], "hex is read in either case");
        assert!(inodes_in("", "0100007F:C34F", "0100007F:1F90").is_empty());
        assert!(inodes_in("garbage\n\n 1: x\n", "0100007F:C34F", "0100007F:1F90").is_empty(), "a row that is not a row is not a match");
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn addresses_are_spelled_as_the_kernel_spells_them() {
        assert_eq!(v4_hex(Ipv4Addr::new(127, 0, 0, 1), 8080), "0100007F:1F90");
        assert_eq!(v4_hex(Ipv4Addr::new(192, 168, 1, 20), 49999), "1401A8C0:C34F");
        assert_eq!(v6_hex(Ipv6Addr::LOCALHOST, 8080), "00000000000000000000000001000000:1F90");
        assert_eq!(v6_hex(Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(), 8080), "0000000000000000FFFF00000100007F:1F90");
    }

    #[test]
    fn a_v4_connection_is_looked_for_in_both_tables_and_a_v6_one_in_the_second() {
        let v4: SocketAddr = "127.0.0.1:49999".parse().unwrap();
        let bound: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let tables: Vec<_> = probes(v4, bound).unwrap().iter().map(|p| p.table).collect();
        assert_eq!(tables, [TCP, TCP6]);
        let v6: SocketAddr = "[::1]:49999".parse().unwrap();
        let bound6: SocketAddr = "[::1]:8080".parse().unwrap();
        let tables: Vec<_> = probes(v6, bound6).unwrap().iter().map(|p| p.table).collect();
        assert_eq!(tables, [TCP6]);
        assert!(probes(v4, bound6).is_none(), "a v4 client and a v6 listener are not one connection");
        // A v4 client that arrived as a v4-mapped v6 address is a v4 one.
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:49999".parse().unwrap();
        assert_eq!(probes(mapped, bound).unwrap().len(), 2);
        // The second probe of a v4 connection is the same connection spelled as v4-mapped v6 addresses.
        let second = probes(v4, bound).unwrap()[1];
        assert_eq!(second.local.to_string(), "[::ffff:127.0.0.1]:49999");
        assert_eq!(second.remote.to_string(), "[::ffff:127.0.0.1]:8080");
    }

    #[test]
    fn a_pid_that_is_not_there_holds_nothing() {
        // The largest pid the kernel can have is below 2^22.
        assert_eq!(find_fd(u32::MAX, b"socket:[1]"), None);
        assert_eq!(find_fd(0, b"socket:[1]"), None);
        assert!(!holds_at(u32::MAX, 3, b"socket:[1]"));
    }

    #[test]
    fn the_hints_are_bounded_and_forget() {
        let hints = Hints::default();
        for inode in 0..Hints::CAP as u64 {
            hints.remember(inode, 1, 3);
        }
        assert_eq!(hints.get(0), Some((1, 3)));
        hints.remember(1 << 40, 2, 4);
        assert_eq!(hints.get(0), None, "a full table is emptied");
        assert_eq!(hints.get(1 << 40), Some((2, 4)));
        hints.forget(1 << 40);
        assert_eq!(hints.get(1 << 40), None);
        assert!(hints.found.lock().unwrap().len() <= Hints::CAP);
    }

    #[test]
    fn a_descriptor_is_found_and_then_asked_after_by_number() {
        // `find_fd` matches a link by its text, so it finds a path as readily as a socket, and a short one fits its buffer.
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file) as u32;
        let want = b"/dev/null";
        let pid = std::process::id();
        // Another descriptor (a harness's stdin) may also be /dev/null; the first one in the directory's order is the answer.
        let found = find_fd(pid, want).expect("this process holds /dev/null");
        assert!(holds_at(pid, found, want));
        assert!(holds_at(pid, fd, want));
        assert!(!holds_at(pid, fd, b"socket:[1]"), "the right number, the wrong thing");
        assert!(!holds_at(pid, 1 << 30, want), "a number that is not open");
    }
}
