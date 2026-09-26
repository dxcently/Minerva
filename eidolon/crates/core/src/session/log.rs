//! The framed, append-only binary log.
//!
//! ## Why binary, why this framing
//!
//! JSONL was rejected (vault, 2026-09-03): nothing casually `cat`s this file,
//! and the one time an LLM read a harness's own log it was unprompted. SQLite
//! was rejected as heavier than the win. What is left is a flat file of
//! length-prefixed, checksummed records — the same crash-resume discipline as
//! JSONL's "drop the truncated last line", made explicit:
//!
//! ```text
//! file  := magic version record*
//! magic := b"EIDO"            (4 bytes)
//! version := u16 LE           (1)
//! record := len:u32 LE  crc32:u32 LE  body[len]
//! body  := bitcode::encode(T)
//! ```
//!
//! On open, records are read until the file ends. A **torn** tail (fewer
//! bytes than `len` promises) is discarded and the file is truncated to the
//! last good record, so the next append is well-framed. A **corrupt** body
//! (checksum mismatch on a complete-length record) is an error, not silently
//! skipped: it means something other than a crash touched the file.
//!
//! ## The encoder is a detail
//!
//! `bitcode` is the first choice, `postcard` the fallback. The framing does
//! not care: switching encoders means bumping `VERSION` and re-encoding
//! bodies, not redesigning the file. The generic `T` keeps this module
//! ignorant of what a record *is*; `super` supplies that.
//!
//! ## Head
//!
//! The log tracks a *head*: the record the next append will point to as its
//! parent. It is in-memory state — on open it is the last record in the
//! file, which is always the most recent thing that happened. Forking is
//! `set_head` to an older id; the parent pointer in the record body is what
//! persists the tree, not the head.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

const MAGIC: &[u8; 4] = b"EIDO";
const VERSION: u16 = 1;
const HEADER_LEN: u64 = 6;
/// Upper bound on one record body. A record is one message or one tool
/// result; anything larger than this is a bug, not data.
const MAX_RECORD: u32 = 64 * 1024 * 1024;

pub type RecordId = u64;

/// Why [`SessionLog::open`] refused a file before reading a single record.
///
/// Typed, and reachable through the `anyhow` error `open` returns by
/// `downcast_ref`, because a refusal is data a caller acts on rather than
/// prose it repeats. The caller in question is whoever receives a session
/// shipped from another machine — see [`super::Session::open_projected`] —
/// and the distinction it needs is "this build does not read that
/// version, fetch one that does" from "this is not a session log at all";
/// a string error would make every receiver parse sentences to tell them
/// apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The leading bytes are not the magic. Not a session log at all —
    /// a different file, or a transfer that never arrived.
    NotASessionLog {
        /// The file that was refused.
        path: PathBuf,
    },
    /// A session log, but written by a build whose `VERSION` this one
    /// does not read. Nothing was decoded and the file was not touched:
    /// a hop across versions refuses whole, before any record of it is
    /// trusted.
    VersionMismatch {
        /// The file that was refused.
        path: PathBuf,
        /// The version the header carries.
        found: u16,
        /// The version this build reads.
        supported: u16,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotASessionLog { path } => {
                write!(f, "{} is not a session log (bad magic)", path.display())
            }
            Refusal::VersionMismatch {
                path,
                found,
                supported,
            } => write!(
                f,
                "{}: session log version {found} is not supported (this build reads {supported})",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// Anything the log stores must expose its own id and parent so the head
/// can be tracked generically.
pub trait Framed: bitcode::Encode + bitcode::DecodeOwned {
    fn id(&self) -> RecordId;
}

impl Framed for super::Record {
    fn id(&self) -> RecordId {
        self.id
    }
}

pub struct SessionLog<T> {
    path: PathBuf,
    file: File,
    records: Vec<T>,
    head: Option<RecordId>,
}

/// Header check and checksum-verified replay, shared by both opens.
/// Returns the whole records, the offset the last whole one ends at, and
/// the file's full length — the gap between the last two, when there is
/// one, is the torn tail, and what each open does with that gap is the
/// whole difference between them.
fn replay<T: Framed>(file: &File, path: &Path) -> anyhow::Result<(Vec<T>, u64, u64)> {
    let mut reader = BufReader::new(file);
    let mut header = [0u8; HEADER_LEN as usize];
    reader
        .read_exact(&mut header)
        .context("session log too short for a header")?;
    if &header[..4] != MAGIC {
        return Err(Refusal::NotASessionLog {
            path: path.to_path_buf(),
        }
        .into());
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(Refusal::VersionMismatch {
            path: path.to_path_buf(),
            found: version,
            supported: VERSION,
        }
        .into());
    }

    let total = file.metadata()?.len();
    let mut records = Vec::new();
    let mut good_end = HEADER_LEN;
    let mut pos = HEADER_LEN;
    loop {
        let mut frame = [0u8; 8];
        match reader.read_exact(&mut frame) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
        let crc = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
        if len > MAX_RECORD {
            bail!("record at offset {pos} claims {len} bytes; the log is corrupt");
        }
        if pos + 8 + len as u64 > total {
            // Torn write: the crash happened mid-append. Drop it.
            tracing::warn!(offset = pos, "discarding torn record at end of session log");
            break;
        }
        let mut body = vec![0u8; len as usize];
        reader.read_exact(&mut body)?;
        if crc32fast::hash(&body) != crc {
            bail!("record at offset {pos} fails its checksum; the log is corrupt");
        }
        let rec: T = bitcode::decode(&body)
            .with_context(|| format!("decoding record at offset {pos}"))?;
        records.push(rec);
        pos += 8 + len as u64;
        good_end = pos;
    }
    Ok((records, good_end, total))
}

impl<T: Framed> SessionLog<T> {
    /// Create a fresh log. Refuses to overwrite an existing file.
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("creating session log {}", path.display()))?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.sync_data()?;
        Ok(SessionLog {
            path: path.to_path_buf(),
            file,
            records: Vec::new(),
            head: None,
        })
    }

    /// Open and replay for a session about to *run*: a torn tail from a
    /// crash mid-append is repaired away (truncated) before this process
    /// can append past it, and a corrupt body is an error. Readers that
    /// will never write take [`SessionLog::open_readonly`] — repairing a
    /// *live* log under its running writer truncates the record being
    /// appended, and every append after the cut lands past a gap no future
    /// open can repair.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening session log {}", path.display()))?;
        let (records, good_end, total) = replay::<T>(&file, path)?;
        if good_end < total {
            file.set_len(good_end)?;
        }
        file.seek(SeekFrom::End(0))?;
        let head = records.last().map(|r| r.id());
        Ok(SessionLog {
            path: path.to_path_buf(),
            file,
            records,
            head,
        })
    }

    /// Open and replay for reading, leaving the file byte-for-byte as
    /// found: a torn tail reads as absent (the record was never whole), a
    /// corrupt body is still an error, and nothing is ever truncated. Safe
    /// against a writer appending concurrently — the reader sees each
    /// record whole or not at all, and cannot mutate what it read. The
    /// handle is read-only in the OS sense too, so an accidental append
    /// fails loudly rather than silently working.
    pub fn open_readonly(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("opening session log {}", path.display()))?;
        let (records, _, _) = replay::<T>(&file, path)?;
        let head = records.last().map(|r| r.id());
        Ok(SessionLog {
            path: path.to_path_buf(),
            file,
            records,
            head,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn head(&self) -> Option<RecordId> {
        self.head
    }

    /// The id the next appended record must carry.
    pub fn next_id(&self) -> RecordId {
        self.records.len() as RecordId
    }

    pub fn records(&self) -> &[T] {
        &self.records
    }

    pub fn get(&self, id: RecordId) -> Option<&T> {
        self.records.get(id as usize)
    }

    /// Append one record. Its `id()` must equal `next_id()`; the caller sets
    /// the parent. Becomes the new head. Durable before returning.
    pub fn append(&mut self, rec: T) -> anyhow::Result<RecordId> {
        let id = rec.id();
        if id != self.next_id() {
            bail!(
                "record id {id} out of sequence (expected {})",
                self.next_id()
            );
        }
        let body = bitcode::encode(&rec);
        if body.len() as u64 > MAX_RECORD as u64 {
            bail!(
                "record {id} is {} bytes, over the {MAX_RECORD} byte limit",
                body.len()
            );
        }
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
        frame.extend_from_slice(&body);
        self.file
            .write_all(&frame)
            .context("appending to session log")?;
        self.file.sync_data().context("syncing session log")?;
        self.records.push(rec);
        self.head = Some(id);
        Ok(id)
    }

    pub fn set_head(&mut self, id: RecordId) -> anyhow::Result<()> {
        if self.get(id).is_none() {
            bail!("no record {id} to set as head");
        }
        self.head = Some(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, bitcode::Encode, bitcode::Decode)]
    struct R {
        id: u64,
        parent: Option<u64>,
        text: String,
    }
    impl Framed for R {
        fn id(&self) -> RecordId {
            self.id
        }
    }

    #[test]
    fn roundtrip_and_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.log");
        {
            let mut log = SessionLog::<R>::create(&p).unwrap();
            log.append(R {
                id: 0,
                parent: None,
                text: "a".into(),
            })
            .unwrap();
            log.append(R {
                id: 1,
                parent: Some(0),
                text: "b".repeat(1000),
            })
            .unwrap();
        }
        // Tear the last record.
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..bytes.len() - 100]).unwrap();
        let mut log = SessionLog::<R>::open(&p).unwrap();
        assert_eq!(log.records().len(), 1);
        assert_eq!(log.head(), Some(0));
        // Appending after truncation is well-framed.
        log.append(R {
            id: 1,
            parent: Some(0),
            text: "c".into(),
        })
        .unwrap();
        drop(log);
        let log = SessionLog::<R>::open(&p).unwrap();
        assert_eq!(log.records().len(), 2);
        assert_eq!(log.records()[1].text, "c");
    }

    #[test]
    fn corrupt_body_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.log");
        {
            let mut log = SessionLog::<R>::create(&p).unwrap();
            log.append(R {
                id: 0,
                parent: None,
                text: "hello".into(),
            })
            .unwrap();
        }
        let mut bytes = std::fs::read(&p).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&p, &bytes).unwrap();
        assert!(SessionLog::<R>::open(&p).is_err());
    }

    #[test]
    fn fork_via_set_head() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.log");
        let mut log = SessionLog::<R>::create(&p).unwrap();
        log.append(R {
            id: 0,
            parent: None,
            text: "root".into(),
        })
        .unwrap();
        log.append(R {
            id: 1,
            parent: Some(0),
            text: "a".into(),
        })
        .unwrap();
        log.set_head(0).unwrap();
        log.append(R {
            id: 2,
            parent: log.head(),
            text: "b".into(),
        })
        .unwrap();
        assert_eq!(log.get(2).unwrap().parent, Some(0));
    }
}
