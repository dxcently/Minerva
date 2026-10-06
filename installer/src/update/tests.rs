use super::*;
use crate::install::testbundle;
use crate::keys::tests::{id_of, rotation, trust_with};
use crate::verify::testkeys::{keypair, sign};

const TODAY: &str = "2026-10-06";
const NOW: u64 = 1_791_269_513; // 2026-10-06
const DAY: u64 = 86_400;

struct Rel {
    _t: tempfile::TempDir,
    dir: PathBuf,
    base: String,
}

/// A release folder for `full` signed with `sk`.
fn release(full: &str, sk: &minisign::SecretKey) -> Rel {
    release_with(full, sk, "")
}

/// Same, with extra top-level manifest members, e.g. `,"released":1,"keys_serial":2`.
fn release_with(full: &str, sk: &minisign::SecretKey, extra: &str) -> Rel {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().to_path_buf();
    let name = format!("minerva-{full}-linux-x86_64.tar.gz");
    let tgz = testbundle::targz(full);
    fs::write(dir.join(&name), &tgz).unwrap();
    fs::write(dir.join(format!("{name}.minisig")), sign(sk, &tgz)).unwrap();
    let m = format!(
        r#"{{"schema":1,"version":"{}","full":"{full}"{extra},"bundle":{{"name":"{name}","sha256":"{}","size":{}}}}}"#,
        full.split('-').next().unwrap(),
        hex(&Sha256::digest(&tgz)),
        tgz.len()
    );
    fs::write(dir.join("latest.json"), &m).unwrap();
    fs::write(dir.join("latest.json.minisig"), sign(sk, m.as_bytes())).unwrap();
    let base = format!("file://{}", dir.display());
    Rel { _t: t, dir, base }
}

fn with_rotation(rel: &Rel, json: &str, sk: &minisign::SecretKey) {
    fs::write(rel.dir.join("keys.json"), json).unwrap();
    fs::write(rel.dir.join("keys.json.minisig"), sign(sk, json.as_bytes())).unwrap();
}

fn installed(prefix: &Path, full: &str) -> (Layout, PathBuf) {
    let l = Layout::new(prefix.canonicalize().unwrap());
    let s = prefix.join(format!("seed-{full}"));
    testbundle::make(&s, full);
    install::install_from_dir(&s, &l).unwrap();
    let exe = l.current().join("bin/minerva");
    (l, exe)
}

fn go(base: &str, trust: &Trust, cur: &str, exe: &Path, rollback: bool) -> (Result<Outcome>, String) {
    go_at(NOW, base, trust, cur, exe, rollback)
}

fn go_at(now: u64, base: &str, trust: &Trust, cur: &str, exe: &Path, rollback: bool) -> (Result<Outcome>, String) {
    let mut out = Vec::new();
    let r = run(&Opts { base, trust, now, current: cur, exe, rollback, check_only: false }, &mut out);
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn good_signature_installs_then_rollback_flips_back() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, text) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert_eq!(r.unwrap(), Outcome::Updated { from: "0.1.0-gold".into(), to: "0.2.0-gnew".into() });
    assert!(text.contains("signatures ok"));
    assert_eq!(l.current_version().as_deref(), Some("0.2.0-gnew"));
    assert_eq!(l.previous_version().as_deref(), Some("0.1.0-gold"));
    // Now up to date.
    let exe2 = l.current().join("bin/minerva");
    assert_eq!(go(&rel.base, &trust, "0.2.0-gnew", &exe2, false).0.unwrap(), Outcome::UpToDate);
    // Rollback.
    let (r, _) = go(&rel.base, &trust, "0.2.0-gnew", &exe2, true);
    assert_eq!(r.unwrap(), Outcome::RolledBack { from: "0.2.0-gnew".into(), to: "0.1.0-gold".into() });
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
}

#[test]
fn tampered_bundle_is_refused_and_nothing_changes() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk);
    let name = "minerva-0.2.0-gnew-linux-x86_64.tar.gz";
    let mut b = fs::read(rel.dir.join(name)).unwrap();
    let n = b.len() / 2;
    b[n] ^= 0xff;
    fs::write(rel.dir.join(name), b).unwrap();
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, _) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    let e = format!("{:#}", r.unwrap_err());
    assert!(e.contains("FAILED") || e.contains("refusing"), "{e}");
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
    assert!(!l.version_dir("0.2.0-gnew").exists());
    assert!(fs::read_dir(l.opt()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".update")));
}

#[test]
fn wrong_key_and_missing_signature_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let (pk, _) = keypair();
    let (_k, trust) = trust_with(&pk);
    let (_, attacker) = keypair();
    let rel = release("0.2.0-gnew", &attacker);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, _) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert!(format!("{:#}", r.unwrap_err()).contains("not one this program trusts"));
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
    fs::remove_file(rel.dir.join("latest.json.minisig")).unwrap();
    assert!(go(&rel.base, &trust, "0.1.0-gold", &exe, false).0.is_err());
}

#[test]
fn bundle_swapped_for_another_validly_signed_one_is_refused() {
    // The attacker holds a genuinely signed OLD bundle and swaps it in: the manifest's sha-256 catches it.
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk);
    let name = "minerva-0.2.0-gnew-linux-x86_64.tar.gz";
    let old = testbundle::targz("0.0.9-gancient");
    fs::write(rel.dir.join(name), &old).unwrap();
    fs::write(rel.dir.join(format!("{name}.minisig")), sign(&sk, &old)).unwrap();
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, _) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert!(format!("{:#}", r.unwrap_err()).contains("does not match"));
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
}

#[test]
fn nix_install_is_refused() {
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk);
    let (r, _) = go(&rel.base, &trust, "0.1.0", Path::new("/nix/store/abc123-minerva-0.1.0/opt/minerva/x/bin/minerva"), false);
    let e = format!("{:#}", r.unwrap_err());
    assert!(e.contains("nix profile upgrade"), "{e}");
}

#[test]
fn check_only_installs_nothing() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let mut out = Vec::new();
    let r = run(&Opts { base: &rel.base, trust: &trust, now: NOW, current: "0.1.0-gold", exe: &exe, rollback: false, check_only: true }, &mut out);
    assert_eq!(r.unwrap(), Outcome::Available("0.2.0".into()));
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
}

#[test]
fn rotation_in_a_release_then_the_new_key_signs() {
    // The release is signed by the NEW key and carries the rotation (signed by the old, still-trusted one).
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_k, trust) = trust_with(&old_pk);
    let rel = release("0.2.0-gnew", &new_sk);
    with_rotation(&rel, &rotation(1, "2030-01-01", &[&new_pk], &[]), &old_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, text) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert!(matches!(r.unwrap(), Outcome::Updated { .. }));
    assert!(text.contains("key rotation applied: 1 added, 0 revoked"), "{text}");
    assert_eq!(l.current_version().as_deref(), Some("0.2.0-gnew"));
}

#[test]
fn rotation_signed_by_an_unknown_key_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (evil_pk, evil_sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &sk); // the bundle itself is fine
    with_rotation(&rel, &rotation(1, "2030-01-01", &[&evil_pk], &[]), &evil_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, _) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert!(format!("{:#}", r.unwrap_err()).contains("keys.json"));
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
    assert_eq!(trust.trusted(TODAY).unwrap().len(), 1, "the evil key was not added");
    // a keys.json with no signature file is refused too
    fs::remove_file(rel.dir.join("keys.json.minisig")).unwrap();
    assert!(go(&rel.base, &trust, "0.1.0-gold", &exe, false).0.is_err());
}

#[test]
fn after_the_compiled_key_is_revoked_an_old_key_release_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_k, trust) = trust_with(&old_pk);
    // release 0.2.0: signed by the new key, its rotation revokes the old (compiled-in) one
    let rel2 = release("0.2.0-gnew", &new_sk);
    with_rotation(&rel2, &rotation(1, "2030-01-01", &[&new_pk], &[&id_of(&old_pk)]), &old_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    assert!(matches!(go(&rel2.base, &trust, "0.1.0-gold", &exe, false).0.unwrap(), Outcome::Updated { .. }));
    // a later release signed by the OLD key (a stolen key) is refused, with or without a replayed rotation
    let rel3 = release("0.3.0-gstolen", &old_sk);
    let exe2 = l.current().join("bin/minerva");
    let (r, _) = go(&rel3.base, &trust, "0.2.0-gnew", &exe2, false);
    assert!(format!("{:#}", r.unwrap_err()).contains("revoked"));
    with_rotation(&rel3, &rotation(2, "2030-01-01", &[&old_pk], &[]), &old_sk);
    assert!(go(&rel3.base, &trust, "0.2.0-gnew", &exe2, false).0.is_err());
    assert_eq!(l.current_version().as_deref(), Some("0.2.0-gnew"));
    // and the new key still works
    let rel4 = release("0.4.0-gok", &new_sk);
    assert!(matches!(go(&rel4.base, &trust, "0.2.0-gnew", &exe2, false).0.unwrap(), Outcome::Updated { .. }));
}

#[test]
fn manual_add_then_update_works_and_remove_stops_it() {
    let t = tempfile::tempdir().unwrap();
    let (pk, _) = keypair();
    let (extra_pk, extra_sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = release("0.2.0-gnew", &extra_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    assert!(go(&rel.base, &trust, "0.1.0-gold", &exe, false).0.is_err(), "not trusted yet");
    let k = trust.add_manual(&extra_pk).unwrap();
    assert!(matches!(go(&rel.base, &trust, "0.1.0-gold", &exe, false).0.unwrap(), Outcome::Updated { .. }));
    trust.remove(&k.id).unwrap();
    let rel3 = release("0.3.0-gnext", &extra_sk);
    let exe2 = l.current().join("bin/minerva");
    assert!(go(&rel3.base, &trust, "0.2.0-gnew", &exe2, false).0.is_err(), "removed: refused again");
}

#[test]
fn an_applied_revocation_fetched_again_does_not_block_later_updates() {
    // H1: the release keeps carrying the keys.json that revoked the old key, signed BY the old key. Once applied, every
    // later update re-fetches it; it must be "already applied", not "signed by a revoked key".
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_k, trust) = trust_with(&old_pk);
    let rot = rotation(1, "2030-01-01", &[&new_pk], &[&id_of(&old_pk)]);
    let rel2 = release("0.2.0-gnew", &new_sk);
    with_rotation(&rel2, &rot, &old_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    assert!(matches!(go(&rel2.base, &trust, "0.1.0-gold", &exe, false).0.unwrap(), Outcome::Updated { .. }));
    // the identical file again, next to a newer release signed by the new key
    let rel3 = release("0.3.0-gnext", &new_sk);
    with_rotation(&rel3, &rot, &old_sk);
    let exe2 = l.current().join("bin/minerva");
    let (r, text) = go(&rel3.base, &trust, "0.2.0-gnew", &exe2, false);
    assert!(matches!(r.unwrap(), Outcome::Updated { .. }), "{text}");
    assert!(!text.contains("key rotation applied"), "{text}");
}

#[test]
fn an_applied_rotation_past_its_expiry_does_not_block_later_updates() {
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_k, trust) = trust_with(&old_pk);
    let rot = rotation(1, "2026-12-31", &[&new_pk], &[]);
    let rel2 = release("0.2.0-gnew", &new_sk);
    with_rotation(&rel2, &rot, &old_sk);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    assert!(matches!(go(&rel2.base, &trust, "0.1.0-gold", &exe, false).0.unwrap(), Outcome::Updated { .. }));
    let rel3 = release("0.3.0-gnext", &new_sk);
    with_rotation(&rel3, &rot, &old_sk);
    let exe2 = l.current().join("bin/minerva");
    // 120 days later the file is past its date; it was applied long ago
    let (r, text) = go_at(NOW + 120 * DAY, &rel3.base, &trust, "0.2.0-gnew", &exe2, false);
    assert!(matches!(r.unwrap(), Outcome::Updated { .. }), "{text}");
    // but a NEW rotation past its date is still refused
    let rel4 = release("0.4.0-gx", &new_sk);
    with_rotation(&rel4, &rotation(2, "2026-12-31", &[], &[]), &old_sk);
    let exe3 = l.current().join("bin/minerva");
    let e = format!("{:#}", go_at(NOW + 120 * DAY, &rel4.base, &trust, "0.3.0-gnext", &exe3, false).0.unwrap_err());
    assert!(e.contains("expired"), "{e}");
}

#[test]
fn a_stale_manifest_warns_and_still_installs() {
    // M3: 30 days, warn only
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let released = NOW - 40 * DAY;
    let rel = release_with("0.2.0-gnew", &sk, &format!(r#","released":{released},"expires":{}"#, released + crate::manifest::FRESH_SECS));
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let (r, text) = go(&rel.base, &trust, "0.1.0-gold", &exe, false);
    assert!(matches!(r.unwrap(), Outcome::Updated { .. }), "{text}");
    assert!(text.contains("release info is stale (signed 2026-08-27)") && text.contains("re-sign"), "{text}");
    assert_eq!(l.current_version().as_deref(), Some("0.2.0-gnew"));
    // a fresh one says nothing
    let fresh = release_with("0.3.0-gnext", &sk, &format!(r#","released":{NOW},"expires":{}"#, NOW + crate::manifest::FRESH_SECS));
    let exe2 = l.current().join("bin/minerva");
    let (_, text) = go(&fresh.base, &trust, "0.2.0-gnew", &exe2, false);
    assert!(!text.contains("stale"), "{text}");
}

#[test]
fn a_release_that_needs_a_rotation_must_bring_keys_json() {
    // M3: keys_serial above the local serial and no keys.json: refused
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_k, trust) = trust_with(&old_pk);
    let rel = release_with("0.2.0-gnew", &new_sk, r#","keys_serial":1"#);
    // the new key is not trusted yet, so even the signature fails; use a release the old key signs to isolate keys_serial
    let rel_old = release_with("0.2.0-gnew", &old_sk, r#","keys_serial":1"#);
    let (l, exe) = installed(t.path(), "0.1.0-gold");
    let e = format!("{:#}", go(&rel_old.base, &trust, "0.1.0-gold", &exe, false).0.unwrap_err());
    assert!(e.contains("needs key rotation 1") && e.contains("keys.json"), "{e}");
    assert_eq!(l.current_version().as_deref(), Some("0.1.0-gold"));
    // with the rotation attached it goes through
    with_rotation(&rel, &rotation(1, "2030-01-01", &[&new_pk], &[]), &old_sk);
    assert!(matches!(go(&rel.base, &trust, "0.1.0-gold", &exe, false).0.unwrap(), Outcome::Updated { .. }));
    // and once the machine has serial 1, a later release naming serial 1 needs no file
    let rel3 = release_with("0.3.0-gnext", &new_sk, r#","keys_serial":1"#);
    let exe2 = l.current().join("bin/minerva");
    assert!(matches!(go(&rel3.base, &trust, "0.2.0-gnew", &exe2, false).0.unwrap(), Outcome::Updated { .. }));
}

#[test]
fn check_only_judges_the_rotation_in_memory_and_writes_nothing() {
    // L4
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (kt, trust) = trust_with(&old_pk);
    let rel = release("0.2.0-gnew", &new_sk);
    with_rotation(&rel, &rotation(1, "2030-01-01", &[&new_pk], &[&id_of(&old_pk)]), &old_sk);
    let (_l, exe) = installed(t.path(), "0.1.0-gold");
    let mut out = Vec::new();
    let r = run(&Opts { base: &rel.base, trust: &trust, now: NOW, current: "0.1.0-gold", exe: &exe, rollback: false, check_only: true }, &mut out);
    assert_eq!(r.unwrap(), Outcome::Available("0.2.0".into()));
    assert!(!kt.path().join("keys").exists(), "nothing was written: no keys dir, no state");
    assert_eq!(trust.state().unwrap().serial, 0);
}
