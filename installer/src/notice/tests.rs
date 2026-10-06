use super::*;
use crate::keys::tests::trust_with;
use crate::verify::testkeys::{keypair, sign};

fn release(dir: &std::path::Path, ver: &str, sk: &minisign::SecretKey) {
    let m = format!(
        r#"{{"schema":1,"version":"{ver}","full":"{ver}-gx","bundle":{{"name":"b.tar.gz","sha256":"{}","size":1}}}}"#,
        "0".repeat(64)
    );
    fs::write(dir.join("latest.json"), &m).unwrap();
    fs::write(dir.join("latest.json.minisig"), sign(sk, m.as_bytes())).unwrap();
}

#[test]
fn notice_once_a_day_and_silent_on_failure() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = t.path().join("rel");
    fs::create_dir_all(&rel).unwrap();
    release(&rel, "0.2.0", &sk);
    let l = Layout::new(t.path().join("p"));
    let base = format!("file://{}", rel.display());
    let msg = check(&l, &base, &trust, "0.1.0-gx", "hoot", 1_000_000).unwrap();
    assert_eq!(msg, "Minerva 0.2.0 available -- run: hoot update");
    // Within a day the cache answers; the release dir may vanish.
    fs::remove_dir_all(&rel).unwrap();
    assert!(check(&l, &base, &trust, "0.1.0-gx", "minerva", 1_000_500).is_some());
    // A day later the fetch fails: silent, and the old verified answer stays.
    assert!(check(&l, &base, &trust, "0.1.0-gx", "minerva", 1_000_000 + EVERY_SECS + 1).is_some());
    // Up to date: no line.
    assert!(check(&l, &base, &trust, "0.2.0-gx", "minerva", 1_000_600).is_none());
}

#[test]
fn a_bad_signature_gives_no_notice() {
    let t = tempfile::tempdir().unwrap();
    let (pk, _) = keypair();
    let (_k, trust) = trust_with(&pk);
    let (_, other_sk) = keypair();
    let rel = t.path().join("rel");
    fs::create_dir_all(&rel).unwrap();
    release(&rel, "9.9.9", &other_sk);
    let l = Layout::new(t.path().join("p"));
    assert!(check(&l, &format!("file://{}", rel.display()), &trust, "0.1.0", "hoot", 5_000_000).is_none());
}

#[test]
fn the_notice_previews_a_rotation_in_memory_and_writes_nothing() {
    // M4: a release signed by a key only its own keys.json introduces is noticed; nothing is written; a bad rotation is silent
    let t = tempfile::tempdir().unwrap();
    let (old_pk, old_sk) = keypair();
    let (new_pk, new_sk) = keypair();
    let (_evil_pk, evil_sk) = keypair();
    let (kt, trust) = trust_with(&old_pk);
    let rel = t.path().join("rel");
    fs::create_dir_all(&rel).unwrap();
    release(&rel, "0.2.0", &new_sk);
    let json = crate::keys::tests::rotation(1, "2030-01-01", &[&new_pk], &[]);
    fs::write(rel.join("keys.json"), &json).unwrap();
    fs::write(rel.join("keys.json.minisig"), sign(&old_sk, json.as_bytes())).unwrap();
    let l = Layout::new(t.path().join("p"));
    let base = format!("file://{}", rel.display());
    assert_eq!(check(&l, &base, &trust, "0.1.0-gx", "minerva", 1_791_269_513).unwrap(), "Minerva 0.2.0 available -- run: minerva update");
    assert!(!kt.path().join("keys").exists(), "the keys dir was not created");
    // the same release with the rotation signed by an unknown key: no notice
    fs::write(rel.join("keys.json.minisig"), sign(&evil_sk, json.as_bytes())).unwrap();
    let l2 = Layout::new(t.path().join("p2"));
    assert!(check(&l2, &base, &trust, "0.1.0-gx", "minerva", 1_791_269_513).is_none());
}

#[test]
fn the_notice_carries_the_stale_warning() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = t.path().join("rel");
    fs::create_dir_all(&rel).unwrap();
    let m = format!(
        r#"{{"schema":1,"version":"0.2.0","full":"0.2.0-gx","released":1000,"expires":2000,"bundle":{{"name":"b.tar.gz","sha256":"{}","size":1}}}}"#,
        "0".repeat(64)
    );
    fs::write(rel.join("latest.json"), &m).unwrap();
    fs::write(rel.join("latest.json.minisig"), sign(&sk, m.as_bytes())).unwrap();
    let l = Layout::new(t.path().join("p"));
    let msg = check(&l, &format!("file://{}", rel.display()), &trust, "0.1.0-gx", "hoot", 3_000_000).unwrap();
    assert!(msg.contains("available") && msg.contains("release info is stale (signed 1970-01-01)"), "{msg}");
}
