//! Lane claims over the presence registry: the token pool.
//!
//! A provider can declare more than one credential — a *pool* of secret
//! names — when the account behind it issues several keys and the
//! endpoint's caches are scoped per key. ollama.com's prefix cache is the
//! measured case: each API key owns a private pool of warm entries, so two
//! harness sessions sharing one key evict each other's prefixes while the
//! same two sessions on two keys never contend at all. One lane per live
//! session is what turns that measurement into a configuration.
//!
//! ## The claim is a directory, because the registry is a directory
//!
//! There is no daemon to hold pool state, and the posture that made the
//! roster work without one works here too: the presence registry under
//! `$XDG_RUNTIME_DIR/eidolon` *is* the state. A held lane is
//!
//! ```text
//! $XDG_RUNTIME_DIR/eidolon/keypool/<provider>/<lane>/holder   # session id
//! ```
//!
//! and taking one is `create_dir` — atomic, exclusive, multi-writer safe
//! by construction, which a read-modify-write claim file would not be.
//! Two sessions that claim in the same millisecond get two different
//! lanes because the second `mkdir` of the first lane fails, not because
//! either of them locked anything.
//!
//! Sharing the registry's root means sharing its sweep: [`crate::presence::scan`]
//! removes any directory under it that has no `meta.json` and is past the
//! grace period, and a lane has none by construction. It skips
//! [`POOL_DIR`] by name for that reason — without the skip an ordinary
//! `peers` call deleted every lane and the next session took the one a
//! live neighbour was on. Nothing else under this root is exempt, and
//! nothing here should grow a second directory that is not a session.
//!
//! The lane is held for the session's lifetime: sticky to the holder, so
//! switching models away and back (or a policy judge resolved from the
//! same process) re-reads the same claim rather than churning lanes.
//!
//! ## Orphans are reclaimed by liveness, not by cleanup
//!
//! A session that dies without releasing (crash, `kill -9`, a predecessor
//! session glitching mid-work) leaves its lane directory behind, and the
//! `holder` file names a session whose presence directory no longer
//! answers. A claimant that meets a held lane asks about that holder much
//! of what the roster's sweep asks: does the socket answer, and — silent —
//! is the registration young enough to be starting up? Within the grace
//! period a silent holder gets the benefit of the doubt; past it, the lane
//! is reclaimed by whoever wants it. The pool stops there where the sweep
//! goes on to ask the registration's pid — deliberate, and parked as a
//! ruling with Noah (swarm follow-ups, the vault's Eidolon TODOs): a lane
//! past grace whose holder is silent stays reclaimable, so a
//! wedged-but-alive holder cannot pin its lane forever. Nothing polls and
//! nothing sweeps on a timer; the question is asked exactly when a new
//! session wants a lane.
//!
//! ## The two trades, accepted out loud
//!
//! * More concurrent sessions than lanes means sharing: when every lane
//!   is held by a live session the claimant takes the first lane and
//!   *holds nothing* — it re-enters the eviction regime the pool exists
//!   to avoid, and its turns contend with the lane's owner. The
//!   alternative (refusing the session) would brick attended work over a
//!   cache optimization.
//! * Two sessions with similar large prefixes on *different* lanes each
//!   pay the cold-start admission cost the prefix would have shared on
//!   one lane. The pool buys isolation, not deduplication; that is the
//!   deal, and it is the right one while eviction (which sharing causes)
//!   is more expensive than admission (which isolation duplicates).

use std::path::Path;

use crate::presence::GRACE_MS;

/// The pool's own directory under the presence root.
///
/// It lives *inside* the registry deliberately — the registry is the state,
/// and a lane claim is one more directory under it — which means it is also
/// in the path of [`crate::presence::scan`]. That sweep reads a directory
/// with no `meta.json` as debris, and a lane has none by construction, so
/// the name is shared here and the sweep skips it explicitly rather than
/// being left to guess. See [`crate::presence::scan`].
pub const POOL_DIR: &str = "keypool";

/// Claim a lane of `pool` for `holder`, first free lane first.
///
/// Returns `(lane_index, fresh)`: which entry of `lanes` to use, and
/// whether this call created the claim (a sticky re-claim of a lane the
/// holder already holds is not fresh, which is what lets a caller journal
/// the claim once rather than on every model switch). An empty `lanes`
/// is the caller's to guard; here it claims lane 0 of nothing and shares.
pub fn claim(root: &Path, pool: &str, lanes: &[String], holder: &str) -> (usize, bool) {
    if lanes.is_empty() || !safe(holder) || !safe(pool) {
        return (0, false);
    }
    let dir = root.join(POOL_DIR).join(pool);
    if std::fs::create_dir_all(&dir).is_err() {
        return (0, false);
    }
    // Sticky: a lane this holder already holds is re-read, not re-claimed,
    // so the claim survives model switches and second resolutions.
    for (i, lane) in lanes.iter().enumerate() {
        if !safe(lane) {
            continue;
        }
        if holder_of(&dir.join(lane)).as_deref() == Some(holder) {
            return (i, false);
        }
    }
    for (i, lane) in lanes.iter().enumerate() {
        if !safe(lane) {
            continue;
        }
        let lane_dir = dir.join(lane);
        match std::fs::create_dir(&lane_dir) {
            Ok(()) => {
                if write_holder(&lane_dir, holder) {
                    tracing::info!(pool, lane, holder, "key pool lane claimed");
                    return (i, true);
                }
                // The claim exists but does not name itself; the next
                // claimant will reclaim it. Leave it — unwritable now
                // means unwritable then too, and a lane we cannot mark is
                // a lane we cannot hold.
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Held — by whom, and are they alive? A holder whose
                // presence is gone (or silent past the grace period) is
                // dead for claiming purposes — the pool's narrower
                // question; the sweep also asks the pid, a divergence
                // parked as a ruling, see `holder_live`.
                let live = holder_of(&lane_dir)
                    .is_some_and(|h| h != holder && holder_live(root, &h));
                if !live {
                    let _ = std::fs::remove_dir_all(&lane_dir);
                    // The retried create can lose a race with a third
                    // session reclaiming the same lane; losing means the
                    // lane is genuinely held, so fall through to the next.
                    if std::fs::create_dir(&lane_dir).is_ok()
                        && write_holder(&lane_dir, holder)
                    {
                        tracing::info!(pool, lane, holder, "key pool lane reclaimed");
                        return (i, true);
                    }
                }
            }
            Err(_) => {}
        }
    }
    // Every lane is held by a live session: share the first, holding
    // nothing — the accepted trade, see the module docs.
    tracing::info!(pool, holder, "key pool full; sharing the first lane");
    (0, false)
}

/// The `holder` file's contents, if it is readable.
fn holder_of(lane_dir: &Path) -> Option<String> {
    std::fs::read_to_string(lane_dir.join("holder")).ok().map(|s| s.trim().to_string())
}

/// Name the holder inside its claim, atomically: a claim that crashed
/// between the `mkdir` and this write is an unowned lane, which the next
/// claimant reclaims — never a lane silently stuck.
fn write_holder(lane_dir: &Path, holder: &str) -> bool {
    let tmp = lane_dir.join(".holder.tmp");
    std::fs::write(&tmp, holder).is_ok() && std::fs::rename(&tmp, lane_dir.join("holder")).is_ok()
}

/// Is the session `holder` live? The roster's own test, asked for the
/// same reason it is asked there: a file can lie, a socket cannot.
///
/// A presence directory that does not exist is a session that was swept
/// or never registered — dead for claiming purposes. One whose socket
/// answers is live. One whose socket is silent but whose registration is
/// younger than the startup grace gets the benefit of the doubt, the
/// grace half of what [`crate::presence::scan`] gives a registration.
///
/// Past grace the pool calls a silent holder dead, and that is now
/// deliberately narrower than the sweep: since the swept-live-session
/// fix, `scan` pairs silence with the registration's pid
/// (`presence::pid_live`) and deletes only a registration whose process
/// is gone, at any age. The pool does not ask the pid — a lane past grace
/// whose holder is silent stays reclaimable, so a wedged-but-alive holder
/// cannot pin its lane forever — and whether it should ask is the ruling
/// parked with Noah (swarm follow-ups, the vault's Eidolon TODOs). The
/// divergence is the pool's answer, stated here so a reader of the sweep
/// does not mistake it for drift.
fn holder_live(root: &Path, holder: &str) -> bool {
    if !safe(holder) {
        return false;
    }
    let dir = root.join(holder);
    if !dir.is_dir() {
        return false;
    }
    if crate::socket::probe(&dir.join("sock")) {
        return true;
    }
    // No readable meta: fall back to the directory's own age, keeping the
    // directory if even that cannot be read — the same conservatism as
    // the sweep.
    let age = match read_started_ms(&dir) {
        Some(t) => crate::now_ms().saturating_sub(t),
        None => match dir.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
        {
            Some(d) => d.as_millis() as u64,
            None => return true,
        },
    };
    age <= GRACE_MS
}

/// `started_ms` out of a presence `meta.json`, when it parses.
fn read_started_ms(dir: &Path) -> Option<u64> {
    let body = std::fs::read(dir.join("meta.json")).ok()?;
    serde_json::from_slice::<crate::presence::Meta>(&body)
        .ok()
        .map(|m| m.started_ms)
}

/// A path component we minted ourselves: no separators, no `.`/`..`, no
/// emptiness. Session ids, provider names and secret names all pass by
/// construction; the check is for the ones a config file typed.
fn safe(s: &str) -> bool {
    !s.is_empty() && !s.contains(['/', '\\', ':']) && s != "." && s != ".."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::Presence;

    fn lanes() -> Vec<String> {
        ["ollama", "ollama2", "ollama3"].map(String::from).to_vec()
    }

    fn root_of(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let root = tmp.path().join("run");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Two live sessions take two lanes, and the first one is the first
    /// declared — first-free-key, in declaration order.
    #[test]
    fn two_holders_take_two_lanes_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        let a = Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a").unwrap();
        let b = Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b").unwrap();

        let (i, fresh) = claim(&root, "ollama", &lanes(), a.id());
        assert_eq!((i, fresh), (0, true));
        let (i, fresh) = claim(&root, "ollama", &lanes(), b.id());
        assert_eq!((i, fresh), (1, true), "the second session must not share lane 0");
    }

    /// A holder re-claiming gets its own lane back, not fresh: the claim
    /// is for the session's lifetime, and nothing re-journals it.
    #[test]
    fn a_claim_is_sticky_to_its_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        let a = Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a").unwrap();
        assert_eq!(claim(&root, "ollama", &lanes(), a.id()), (0, true));
        assert_eq!(
            claim(&root, "ollama", &lanes(), a.id()),
            (0, false),
            "switching models away and back keeps the lane"
        );
    }

    /// A lane held by a session that died without releasing — its
    /// registration is old and its socket does not answer — is reclaimed
    /// by the next session that wants a lane.
    #[test]
    fn a_dead_holders_lane_is_reclaimed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        // A crashed predecessor: registration on disk, old, no socket.
        let dead = root.join("eidolon-dead-0000");
        std::fs::create_dir_all(dead.join("inbox")).unwrap();
        std::fs::write(
            dead.join("meta.json"),
            serde_json::json!({
                "id": "eidolon-dead-0000", "pid": 1,
                "log": "/dev/null", "cwd": "/tmp", "repo": null,
                "model": "mock", "started_ms": crate::now_ms() - GRACE_MS - 60_000,
                "title": "gone", "busy": false
            })
            .to_string(),
        )
        .unwrap();
        let lane = root.join("keypool/ollama/ollama");
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::write(lane.join("holder"), "eidolon-dead-0000").unwrap();

        let b = Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b").unwrap();
        let (i, fresh) = claim(&root, "ollama", &lanes(), b.id());
        assert_eq!((i, fresh), (0, true), "the dead session's lane is free again");
        assert_eq!(holder_of(&lane).as_deref(), Some(b.id()));
    }

    /// A holder that is merely silent and young — registering right now,
    /// socket not bound yet — keeps its lane: stealing it would be two
    /// sessions on one key, the exact thing the pool exists to prevent.
    #[test]
    fn a_young_silent_holder_is_not_stolen_from() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        // Registered microseconds ago, socket never bound (no test serves
        // one) — inside the grace period.
        let a = Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a").unwrap();
        assert_eq!(claim(&root, "ollama", &lanes(), a.id()), (0, true));

        let b = Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b").unwrap();
        let (i, _) = claim(&root, "ollama", &lanes(), b.id());
        assert_eq!(i, 1, "the starting session's lane is respected");
    }

    /// Every lane held by a live session: the claimant shares the first
    /// and holds nothing — the accepted trade, not an error.
    #[test]
    fn a_full_pool_shares_the_first_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        let ids: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|n| {
                Presence::register(&root, &tmp.path().join(format!("{n}.eid")), &root, "mock", n)
                    .unwrap()
            })
            .collect();
        for p in &ids {
            assert!(claim(&root, "ollama", &lanes(), p.id()).1);
        }
        let d = Presence::register(&root, &tmp.path().join("d.eid"), &root, "mock", "d").unwrap();
        assert_eq!(claim(&root, "ollama", &lanes(), d.id()), (0, false));
    }

    /// The pool lives under the presence root, so the roster's own sweep
    /// walks over it — and a lane has no `meta.json`, which is exactly the
    /// shape the sweep reads as debris. Without the `keypool` skip, one
    /// ordinary `peers` call deleted every lane: a second session then
    /// found lane 0 free and took it while its owner was still live, which
    /// is two sessions on one key and the eviction regime the pool exists
    /// to prevent. This is the regression that shipped.
    #[test]
    fn a_roster_scan_does_not_sweep_the_pool_away() {
        use crate::presence::scan;

        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        let a = Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a").unwrap();
        assert_eq!(claim(&root, "ollama", &lanes(), a.id()), (0, true));

        // A scan sweeps on age, and a claim directory is not refreshed by
        // anything, so make it unambiguously stale: this is the state an
        // untouched pool reaches within the grace period of its first
        // claim, not a contrived one.
        let pool = root.join("keypool");
        std::fs::File::open(&pool)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();

        let _ = scan(&root);
        assert!(pool.exists(), "a `peers` call must not delete the pool");
        assert_eq!(
            holder_of(&pool.join("ollama").join("ollama")).as_deref(),
            Some(a.id()),
            "the live session keeps the lane it claimed"
        );

        // And the consequence the deletion had: the next session must take
        // lane 1, not collide with the live owner of lane 0.
        let b = Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b").unwrap();
        assert_eq!(
            claim(&root, "ollama", &lanes(), b.id()),
            (1, true),
            "a live holder's lane survives an intervening roster scan"
        );
    }

    /// An unowned lane — the writer crashed between `mkdir` and the
    /// `holder` write — is reclaimable, not stuck.
    #[test]
    fn an_unowned_lane_is_reclaimed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_of(&tmp);
        // Lane 0 is properly held by a live session; lane 1 is torn.
        std::fs::create_dir_all(root.join("keypool/ollama/ollama2")).unwrap();
        let a = Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a").unwrap();
        assert_eq!(claim(&root, "ollama", &lanes(), a.id()), (0, true));
        let b = Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b").unwrap();
        assert_eq!(
            claim(&root, "ollama", &lanes(), b.id()),
            (1, true),
            "the torn claim is reclaimed rather than skipped or stuck"
        );
    }
}
