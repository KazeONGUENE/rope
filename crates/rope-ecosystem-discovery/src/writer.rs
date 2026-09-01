//! Safe rewrite of the overlay JSONL file.
//!
//! Contract (from `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` §5):
//!
//! 1. Discovery scanners collect entries in memory + dedup by `id`.
//! 2. Write the full snapshot to `<overlay>.tmp` in a single append pass.
//! 3. `fsync` the tmp file.
//! 4. `rename(tmp, final)` - atomic on the same filesystem (POSIX).
//! 5. Best-effort `fsync` on the parent directory so the rename metadata
//!    is durable across a crash.
//!
//! The `rope-explorer` loader reads the file at startup + on a periodic
//! refresh. Because rename is atomic, it always sees either the previous
//! full snapshot or the new one - never a partial write.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::entry::OverlayEntry;
use crate::error::{DiscoveryError, DiscoveryResult};

/// Result of a rewrite: how many entries landed vs. were suppressed by
/// dedup. The scanner CLI logs this and the operator uses it to reason
/// about whether the run produced fewer entries than expected (a fresh
/// scanner regression) or more (an ecosystem partner starting to expose
/// new projects).
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteSummary {
    /// Number of entries the scanners fed into the writer (may include
    /// duplicates before dedup).
    pub input_count: usize,
    /// Number of entries actually written to disk after dedup.
    pub written_count: usize,
    /// Number of entries suppressed as exact duplicates of an earlier
    /// entry with the same `id`. First-seen wins; the loader has its
    /// own last-wins policy for on-disk conflicts, but we deliberately
    /// prefer first-seen at write time so scan ordering is stable.
    pub deduped_count: usize,
    /// Total bytes written to the on-disk file (post-rename).
    pub bytes_written: u64,
}

/// Rewrite the overlay file atomically.
///
/// If `entries` is empty, an empty file is written (loader treats this
/// as "no overlay, canonical-only"). We deliberately do NOT delete the
/// file - the loader needs to see the file exist so it can distinguish
/// "no overlay configured" (path missing) from "discovery ran and found
/// nothing" (empty file with fresh mtime).
///
/// Dedup policy: first entry with a given lowercase `id` wins. Later
/// entries with the same `id` are dropped silently but counted in
/// `deduped_count`.
///
/// Validation policy: entries are NOT re-validated here. The scanners
/// are expected to have called `crate::validation::validate` before
/// handing them off. This keeps write-time cheap and predictable.
pub fn write_overlay_atomic(
    entries: &[OverlayEntry],
    final_path: &Path,
) -> DiscoveryResult<WriteSummary> {
    // Parent directory must exist. If not, create it - the systemd unit
    // uses `StateDirectory=` which creates the dir at start time, but a
    // manual invocation (`cargo run --bin rope-ecosystem-discovery`) may
    // point at a fresh path.
    if let Some(parent) = final_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DiscoveryError::WriterIo(format!(
                    "create_dir_all({}): {}",
                    parent.display(),
                    e
                ))
            })?;
        }
    }

    // Dedup by lowercase id, keep first.
    let mut seen: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    let mut ordered: Vec<&OverlayEntry> = Vec::with_capacity(entries.len());
    let mut deduped_count = 0usize;
    for e in entries {
        let key = e.id.trim().to_ascii_lowercase();
        if seen.contains_key(&key) {
            deduped_count += 1;
            continue;
        }
        seen.insert(key, ordered.len());
        ordered.push(e);
    }

    // Temp path lives beside the final file so rename() is on the same
    // filesystem (required for atomicity on POSIX). Use PID + nanosecond
    // suffix so two concurrent writers wouldn't collide - production
    // only ever runs one, but tests spawn several.
    let tmp_path = tmp_path_for(final_path);

    let mut bytes_written: u64 = 0;
    {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| {
                DiscoveryError::WriterIo(format!("open tmp {}: {}", tmp_path.display(), e))
            })?;
        let mut buf = BufWriter::new(file);
        for entry in &ordered {
            let line = entry.to_jsonl_line().map_err(|e| {
                DiscoveryError::WriterIo(format!("serialise entry {}: {}", entry.id, e))
            })?;
            buf.write_all(line.as_bytes()).map_err(|e| {
                DiscoveryError::WriterIo(format!("write entry {}: {}", entry.id, e))
            })?;
            bytes_written += line.len() as u64;
        }
        // Flush BufWriter and get the raw File back so we can fsync it.
        let file = buf
            .into_inner()
            .map_err(|e| DiscoveryError::WriterIo(format!("flush BufWriter: {}", e)))?;
        // fsync the file itself. On Linux this promises the bytes reach
        // disk; on macOS it's F_FULLSYNC via a separate call - not
        // strictly required for correctness because the loader tolerates
        // the pre-rename state.
        file.sync_all()
            .map_err(|e| DiscoveryError::WriterIo(format!("fsync tmp: {}", e)))?;
    }

    // Atomic rename. If this fails, the old file is unchanged (or
    // absent) and the tmp file may linger - the next run will overwrite
    // it. The loader also handles a missing file gracefully.
    std::fs::rename(&tmp_path, final_path).map_err(|e| {
        DiscoveryError::WriterIo(format!(
            "rename({} -> {}): {}",
            tmp_path.display(),
            final_path.display(),
            e
        ))
    })?;

    // Best-effort parent dir fsync. Ignored on error - the rename
    // metadata is already durable enough for our purposes.
    if let Some(parent) = final_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }

    Ok(WriteSummary {
        input_count: entries.len(),
        written_count: ordered.len(),
        deduped_count,
        bytes_written,
    })
}

fn tmp_path_for(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ecosystem-overlay.jsonl".to_string());
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let tmp_name = format!(".{}.tmp.{}.{}", file_name, pid, now_nanos);
    match final_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{DiscoveredBy, Status};
    use tempfile::TempDir;

    fn sample_entry(id: &str, source: &str) -> OverlayEntry {
        OverlayEntry {
            id: id.into(),
            name: format!("{} display", id),
            archetype: "infrastructure".into(),
            status: Status::Live,
            discovered_by: DiscoveredBy::HandoverScanner,
            discovery_source: source.into(),
            discovered_at: 1_786_600_000,
            tags: vec![],
            region: None,
            country: None,
            wallet: None,
            stakeholder_url: None,
            description: None,
            asset_count: None,
            sensor_count: None,
            logo_url: None,
            created_at: None,
            visibility: None,
        }
    }

    #[test]
    fn writes_empty_file_when_no_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        let summary = write_overlay_atomic(&[], &path).unwrap();
        assert_eq!(summary.input_count, 0);
        assert_eq!(summary.written_count, 0);
        assert_eq!(summary.deduped_count, 0);
        assert_eq!(summary.bytes_written, 0);
        assert!(path.exists(), "empty overlay file must exist");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "");
    }

    #[test]
    fn writes_entries_one_per_line() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        let entries = vec![
            sample_entry("alpha", "handover:/a.mdc"),
            sample_entry("beta", "handover:/b.mdc"),
        ];
        let summary = write_overlay_atomic(&entries, &path).unwrap();
        assert_eq!(summary.input_count, 2);
        assert_eq!(summary.written_count, 2);
        assert_eq!(summary.deduped_count, 0);
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["id"].is_string());
            assert!(v["name"].is_string());
        }
        assert_eq!(summary.bytes_written, contents.len() as u64);
    }

    #[test]
    fn dedupes_by_lowercase_id_first_wins() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        let mut e2 = sample_entry("ALPHA", "handover:/other.mdc");
        e2.name = "Second".into();
        let entries = vec![
            sample_entry("alpha", "handover:/first.mdc"),
            e2,
            sample_entry("beta", "handover:/b.mdc"),
        ];
        let summary = write_overlay_atomic(&entries, &path).unwrap();
        assert_eq!(summary.input_count, 3);
        assert_eq!(summary.written_count, 2);
        assert_eq!(summary.deduped_count, 1);
        let contents = std::fs::read_to_string(&path).unwrap();
        // First entry's name wins:
        assert!(contents.contains("alpha display"));
        assert!(!contents.contains("Second"));
    }

    #[test]
    fn atomic_replace_preserves_previous_snapshot_on_write_success() {
        // Simulate two writes and verify the file at each moment is a
        // full snapshot, never a partial concatenation.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        write_overlay_atomic(&[sample_entry("first-only", "handover:/1.mdc")], &path).unwrap();
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert!(after_first.contains("first-only"));
        write_overlay_atomic(
            &[
                sample_entry("second-a", "handover:/2a.mdc"),
                sample_entry("second-b", "handover:/2b.mdc"),
            ],
            &path,
        )
        .unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert!(!after_second.contains("first-only"), "old snapshot must be gone");
        assert!(after_second.contains("second-a"));
        assert!(after_second.contains("second-b"));
    }

    #[test]
    fn creates_parent_directory_when_absent() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("does").join("not").join("exist");
        let path = deep.join("overlay.jsonl");
        assert!(!deep.exists());
        write_overlay_atomic(&[sample_entry("id-a", "handover:/a.mdc")], &path).unwrap();
        assert!(path.exists());
        assert!(deep.is_dir());
    }

    #[test]
    fn no_tmp_files_left_after_successful_write() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        write_overlay_atomic(&[sample_entry("id-a", "handover:/a.mdc")], &path).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|r| r.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        // Exactly the overlay file, no dot-tmp siblings:
        assert_eq!(entries, vec!["overlay.jsonl"]);
    }

    #[test]
    fn round_trip_through_loader_shape() {
        // The written bytes must deserialise cleanly to the shape the
        // loader expects. If this test breaks, the loader will reject
        // production entries.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("overlay.jsonl");
        let mut entry = sample_entry("round-trip", "handover:/rt.mdc");
        entry.wallet = Some("0x1234567890abcdef1234567890abcdef12345678".into());
        entry.stakeholder_url = Some("https://example.com".into());
        entry.description = Some("A description".into());
        entry.tags = vec!["one".into(), "two".into()];
        write_overlay_atomic(&[entry], &path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        // Parse as generic JSON (mirrors the loader's serde_json path):
        let value: serde_json::Value = serde_json::from_str(raw.trim_end()).unwrap();
        assert_eq!(value["id"], "round-trip");
        assert_eq!(value["discovered_by"], "handover-scanner");
        assert_eq!(value["status"], "live");
        assert_eq!(value["archetype"], "infrastructure");
        assert_eq!(value["wallet"], "0x1234567890abcdef1234567890abcdef12345678");
        assert_eq!(value["stakeholder_url"], "https://example.com");
        assert_eq!(value["description"], "A description");
        let tags = value["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], "one");
    }
}
