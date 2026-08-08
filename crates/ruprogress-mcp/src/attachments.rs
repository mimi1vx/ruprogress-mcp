//! The local attachment store: a `{uuid}/{sanitised_basename}`
//! on-disk layout behind an in-memory
//! `HashMap<Uuid, StoredFile>`, plus a background sweeper.
//!
//! No MCP tool calls into this module yet — `get_redmine_attachment`
//! streams a download into [`AttachmentStore::reserve`]/[`AttachmentStore::commit`],
//! and `upload_file`/`cleanup_attachment_files` are the other consumers.
//! Disk-writing lives here and nowhere in `redmine-client`:
//! that crate's `download_attachment` returns a byte stream and nothing else.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::config::AttachmentConfig;

/// Longest basename we will write to disk, independent of the platform's own
/// filename limit — long enough for any real Redmine filename, short enough
/// to never trip `ENAMETOOLONG` on a filesystem with a tighter cap.
const MAX_BASENAME_LEN: usize = 200;

/// A file the server has downloaded from Redmine and is temporarily serving.
#[derive(Debug, Clone)]
pub struct StoredFile {
    pub uuid: Uuid,
    pub attachment_id: u64,
    /// The sanitised basename actually written to disk — see
    /// [`sanitize_basename`]. Load-bearing for stdio clients that hand
    /// `file_path` to another local tool dispatching on the extension.
    pub filename: String,
    pub content_type: Option<String>,
    pub size: u64,
    pub path: PathBuf,
    /// Wall-clock expiry, for API responses (`get_redmine_attachment`'s
    /// `expires_at`).
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Monotonic clock captured at `commit` time, checked by every
    /// [`AttachmentStore::get`] independently of the sweeper's interval:
    /// the sweeper reclaims disk space between lookups, but
    /// must not be the thing standing between a caller and an accurate
    /// "expired" answer.
    created_at: Instant,
}

/// A reserved-but-not-yet-registered download slot: the UUID directory has
/// been created and the sanitised destination path chosen, but nothing has
/// been written yet. The caller streams bytes to `path`, then calls
/// [`AttachmentStore::commit`] on success or [`AttachmentStore::abort`] on
/// failure or a mid-stream cap trip.
#[derive(Debug)]
pub struct Reservation {
    pub uuid: Uuid,
    pub attachment_id: u64,
    pub path: PathBuf,
}

/// Result of a sweep pass, used by the background sweeper and by
/// `cleanup_attachment_files`'s `{cleaned_files, cleaned_bytes, cleaned_mb}`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SweepResult {
    pub removed_files: u64,
    pub removed_bytes: u64,
}

/// Keeps only the last [`Component::Normal`] of `name` (stripping any
/// `RootDir`/`Prefix`/`CurDir`/`ParentDir` component the *host* platform's
/// separator rules would recognise), then strips control
/// characters and caps the length. A name that sanitises to nothing becomes
/// literally `"attachment"`.
///
/// A literal backslash surviving into the result is not a traversal risk on
/// a Unix store: `\` is not a path separator there, so it is inert as a
/// single filesystem entry name.
fn sanitize_basename(name: &str) -> String {
    let mut chosen: Option<std::ffi::OsString> = None;
    for component in Path::new(name).components() {
        if let Component::Normal(part) = component {
            chosen = Some(part.to_os_string());
        }
    }
    let raw = chosen
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default();
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let truncated: String = cleaned.trim().chars().take(MAX_BASENAME_LEN).collect();
    if truncated.is_empty() {
        "attachment".to_string()
    } else {
        truncated
    }
}

/// Sets a directory to `0700` on Unix. A no-op (plus a one-time `WARN` from
/// the caller) on other platforms — see [`AttachmentConfig`].
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Recursively sums file sizes under `path`. Used only by the sweep path to
/// report `removed_bytes` before the directory is deleted; not on any
/// request-serving path.
async fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

/// The local attachment store: on-disk files plus the in-memory index that
/// is the source of truth for serving them.
#[derive(Debug)]
pub struct AttachmentStore {
    dir: PathBuf,
    expires_after: Duration,
    max_download_bytes: u64,
    max_store_bytes: u64,
    entries: Mutex<HashMap<Uuid, StoredFile>>,
}

impl AttachmentStore {
    /// Creates `ATTACHMENTS_DIR` (`0700` on Unix; a `WARN` on other
    /// platforms, since permissions there depend on inherited ACLs).
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be created or (on Unix) its permissions
    /// cannot be set.
    pub fn init(config: &AttachmentConfig) -> std::io::Result<Self> {
        std::fs::create_dir_all(&config.dir)?;
        restrict_to_owner(&config.dir)?;
        #[cfg(not(unix))]
        tracing::warn!(
            dir = %config.dir.display(),
            "attachments directory permissions rely on inherited ACLs on this platform"
        );
        Ok(Self {
            dir: config.dir.clone(),
            expires_after: config.expires_after,
            max_download_bytes: config.max_download_bytes,
            max_store_bytes: config.store_max_bytes,
            entries: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub const fn max_download_bytes(&self) -> u64 {
        self.max_download_bytes
    }

    /// Sum of every currently-tracked entry's size. Cheap: reads the
    /// in-memory map, never the filesystem.
    pub async fn total_bytes(&self) -> u64 {
        self.entries.lock().await.values().map(|e| e.size).sum()
    }

    /// Whether `additional_bytes` more would still fit under
    /// `ATTACHMENT_STORE_MAX_BYTES`. Callers should sweep expired
    /// entries first if this returns `false`, then re-check before refusing
    /// with `STORE_FULL`.
    pub async fn has_room_for(&self, additional_bytes: u64) -> bool {
        self.total_bytes().await.saturating_add(additional_bytes) <= self.max_store_bytes
    }

    /// Allocates a fresh UUID directory and a sanitised destination path
    /// inside it. Does not create or write the file itself — the caller
    /// streams bytes to [`Reservation::path`].
    ///
    /// # Errors
    ///
    /// Fails if the UUID directory cannot be created.
    pub async fn reserve(
        &self,
        attachment_id: u64,
        redmine_filename: &str,
    ) -> std::io::Result<Reservation> {
        let uuid = Uuid::new_v4();
        let entry_dir = self.dir.join(uuid.to_string());
        tokio::fs::create_dir_all(&entry_dir).await?;
        restrict_to_owner(&entry_dir)?;
        let path = entry_dir.join(sanitize_basename(redmine_filename));
        Ok(Reservation {
            uuid,
            attachment_id,
            path,
        })
    }

    /// Registers a completed download, making it fetchable via [`Self::get`].
    pub async fn commit(
        &self,
        reservation: Reservation,
        content_type: Option<String>,
        size: u64,
    ) -> StoredFile {
        let filename = reservation
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let ttl = chrono::Duration::from_std(self.expires_after).unwrap_or(chrono::Duration::MAX);
        let expires_at = chrono::Utc::now()
            .checked_add_signed(ttl)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
        let stored = StoredFile {
            uuid: reservation.uuid,
            attachment_id: reservation.attachment_id,
            filename,
            content_type,
            size,
            path: reservation.path,
            expires_at,
            created_at: Instant::now(),
        };
        self.entries
            .lock()
            .await
            .insert(stored.uuid, stored.clone());
        stored
    }

    /// Discards a reservation that will never be committed (a mid-stream cap
    /// trip or a write error), removing the whole UUID directory.
    pub async fn abort(&self, reservation: &Reservation) {
        if let Some(entry_dir) = reservation.path.parent() {
            let _ = tokio::fs::remove_dir_all(entry_dir).await;
        }
    }

    /// Looks up a stored file by UUID. `None` for an unknown UUID *and* for
    /// one whose TTL has passed — the latter case removes the entry
    /// (map and disk) immediately rather than waiting for the sweeper.
    pub async fn get(&self, id: Uuid) -> Option<StoredFile> {
        let mut entries = self.entries.lock().await;
        let expired = entries
            .get(&id)
            .is_some_and(|e| e.created_at.elapsed() >= self.expires_after);
        if !expired {
            return entries.get(&id).cloned();
        }
        let removed = entries.remove(&id);
        drop(entries);
        if let Some(removed) = removed
            && let Some(entry_dir) = removed.path.parent()
        {
            let _ = tokio::fs::remove_dir_all(entry_dir).await;
        }
        None
    }

    /// Walks `ATTACHMENTS_DIR` directly and removes every subdirectory whose
    /// mtime is at least `expires_after` old: this is the one
    /// mechanism that reclaims disk space both during normal operation and
    /// for a predecessor process's orphans after a restart, since the
    /// in-memory map starts empty either way.
    pub async fn sweep_expired(&self) -> SweepResult {
        let mut result = SweepResult::default();
        let mut read_dir = match tokio::fs::read_dir(&self.dir).await {
            Ok(rd) => rd,
            Err(error) => {
                tracing::warn!(
                    %error,
                    dir = %self.dir.display(),
                    "failed to read the attachments directory during a sweep"
                );
                return result;
            }
        };

        let mut expired_uuids = Vec::new();
        loop {
            let entry = match read_dir.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "failed to read a directory entry during a sweep");
                    break;
                }
            };
            let path = entry.path();
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            // `DirEntry::metadata` does not follow a symlink, so a symlinked
            // "directory" here reports as neither: skipped, not traversed.
            if !metadata.is_dir() {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::ZERO);
            if age < self.expires_after {
                continue;
            }
            let bytes = dir_size(&path).await;
            if tokio::fs::remove_dir_all(&path).await.is_ok() {
                result.removed_files = result.removed_files.saturating_add(1);
                result.removed_bytes = result.removed_bytes.saturating_add(bytes);
                if let Some(uuid) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    expired_uuids.push(uuid);
                }
            }
        }

        if !expired_uuids.is_empty() {
            let mut entries = self.entries.lock().await;
            for uuid in expired_uuids {
                entries.remove(&uuid);
            }
        }
        result
    }
}

/// Spawns the background sweeper on a `TaskTracker`, ticking
/// [`AttachmentStore::sweep_expired`] every `interval` until `ct` is
/// cancelled. The caller owns both: cancel `ct` then `tracker.wait().await`
/// (after `tracker.close()`) to join it cleanly on shutdown. Not started at
/// all when `AUTO_CLEANUP_ENABLED=false` — the caller decides that.
#[must_use]
pub fn spawn_sweeper(
    store: std::sync::Arc<AttachmentStore>,
    interval: Duration,
    ct: CancellationToken,
) -> TaskTracker {
    let tracker = TaskTracker::new();
    tracker.spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = ct.cancelled() => break,
                _ = ticker.tick() => {
                    let result = store.sweep_expired().await;
                    if result.removed_files > 0 {
                        tracing::info!(
                            removed_files = result.removed_files,
                            removed_bytes = result.removed_bytes,
                            "swept expired attachment files"
                        );
                    }
                }
            }
        }
    });
    tracker.close();
    tracker
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn test_config(dir: &Path) -> AttachmentConfig {
        AttachmentConfig {
            dir: dir.to_path_buf(),
            max_download_bytes: 1024 * 1024,
            store_max_bytes: 10 * 1024 * 1024,
            auto_cleanup_enabled: true,
            cleanup_interval: Duration::from_mins(1),
            expires_after: Duration::from_millis(50),
            upload_file_roots: Vec::new(),
            expose_admin_tools: false,
            public_url_rewrite: None,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ruprogress-mcp-test-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    #[test]
    fn sanitize_basename_strips_traversal_and_keeps_the_last_segment() {
        assert_eq!(sanitize_basename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_basename("/etc/passwd"), "passwd");
        assert_eq!(sanitize_basename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_basename(".."), "attachment");
        assert_eq!(sanitize_basename("."), "attachment");
        assert_eq!(sanitize_basename(""), "attachment");
        // A literal backslash is not a separator on this (Unix) host: it
        // stays inert as part of a single filename.
        assert_eq!(sanitize_basename("..\\..\\evil.txt"), "..\\..\\evil.txt");
    }

    #[test]
    fn sanitize_basename_strips_control_characters_and_caps_length() {
        assert_eq!(sanitize_basename("a\0b\nc"), "abc");
        let long = "a".repeat(1000);
        assert_eq!(sanitize_basename(&long).len(), MAX_BASENAME_LEN);
    }

    #[tokio::test]
    async fn init_creates_the_directory() {
        let dir = temp_dir("init");
        let config = test_config(&dir);
        let store = AttachmentStore::init(&config).expect("init should succeed");
        assert!(store.dir().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn reserve_commit_and_get_round_trip() {
        let dir = temp_dir("roundtrip");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();

        let reservation = store.reserve(42, "report.pdf").await.unwrap();
        tokio::fs::write(&reservation.path, b"hello").await.unwrap();
        let uuid = reservation.uuid;
        let stored = store
            .commit(reservation, Some("application/pdf".to_string()), 5)
            .await;
        assert_eq!(stored.filename, "report.pdf");
        assert_eq!(stored.attachment_id, 42);

        let fetched = store.get(uuid).await.expect("should be fetchable");
        assert_eq!(fetched.uuid, uuid);
        assert_eq!(fetched.size, 5);
        assert_eq!(tokio::fs::read(&fetched.path).await.unwrap(), b"hello");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn a_traversal_filename_lands_inside_the_uuid_directory() {
        let dir = temp_dir("traversal");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();

        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "..\\..\\windows\\system32",
            "a\0b",
        ] {
            let reservation = store.reserve(1, hostile).await.unwrap();
            assert!(
                reservation.path.starts_with(&dir),
                "{hostile:?} escaped the attachments directory: {}",
                reservation.path.display()
            );
            // Exactly one path component below the uuid directory: the
            // sanitised basename, not a re-created subtree.
            let entry_dir = reservation.path.parent().unwrap();
            assert_eq!(entry_dir.parent().unwrap(), dir);
        }

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn get_returns_none_for_an_unknown_uuid() {
        let dir = temp_dir("unknown");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();
        assert!(store.get(Uuid::new_v4()).await.is_none());
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn get_expires_an_entry_before_the_sweeper_would_run() {
        let dir = temp_dir("lazy-expiry");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();

        let reservation = store.reserve(1, "f.txt").await.unwrap();
        tokio::fs::write(&reservation.path, b"x").await.unwrap();
        let uuid = reservation.uuid;
        let entry_dir = reservation.path.parent().unwrap().to_path_buf();
        store.commit(reservation, None, 1).await;

        // expires_after is 50ms in test_config; outlast it without relying
        // on the (much longer, interval-based) sweeper.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(store.get(uuid).await.is_none());
        assert!(
            !entry_dir.exists(),
            "a lazily-expired entry's directory should be removed too"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn abort_removes_the_reservation_directory() {
        let dir = temp_dir("abort");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();

        let reservation = store.reserve(1, "f.txt").await.unwrap();
        tokio::fs::write(&reservation.path, b"partial")
            .await
            .unwrap();
        let entry_dir = reservation.path.parent().unwrap().to_path_buf();
        assert!(entry_dir.exists());

        store.abort(&reservation).await;
        assert!(!entry_dir.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn sweep_removes_a_stale_orphan_directory_with_an_empty_map() {
        // Simulates a restarted process: the directory predates this
        // `AttachmentStore` entirely, so the in-memory map has never heard
        // of it, yet the sweep must still reclaim it via mtime.
        let dir = temp_dir("orphan-sweep");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let orphan_uuid = Uuid::new_v4();
        let orphan_dir = dir.join(orphan_uuid.to_string());
        tokio::fs::create_dir_all(&orphan_dir).await.unwrap();
        tokio::fs::write(orphan_dir.join("old.txt"), b"stale bytes")
            .await
            .unwrap();

        // Backdate the directory's mtime well past the TTL. `File::open` on
        // a directory is read-only-but-valid on Unix, enough to retarget its
        // mtime via `set_modified` without adding a dependency for one test.
        let stale = SystemTime::now() - Duration::from_hours(1);
        std::fs::File::open(&orphan_dir)
            .unwrap()
            .set_modified(stale)
            .unwrap();

        let store = AttachmentStore::init(&test_config(&dir)).unwrap();
        let result = store.sweep_expired().await;

        assert_eq!(result.removed_files, 1);
        assert_eq!(result.removed_bytes, "stale bytes".len() as u64);
        assert!(!orphan_dir.exists());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn sweep_leaves_a_fresh_directory_alone() {
        let dir = temp_dir("fresh-sweep");
        let store = AttachmentStore::init(&test_config(&dir)).unwrap();
        let reservation = store.reserve(1, "f.txt").await.unwrap();
        tokio::fs::write(&reservation.path, b"x").await.unwrap();
        let uuid = reservation.uuid;
        store.commit(reservation, None, 1).await;

        let result = store.sweep_expired().await;
        assert_eq!(result.removed_files, 0);
        assert!(store.get(uuid).await.is_some());

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn total_bytes_and_has_room_for_reflect_committed_entries() {
        let dir = temp_dir("capacity");
        let mut config = test_config(&dir);
        config.store_max_bytes = 100;
        let store = AttachmentStore::init(&config).unwrap();

        assert_eq!(store.total_bytes().await, 0);
        assert!(store.has_room_for(100).await);
        assert!(!store.has_room_for(101).await);

        let reservation = store.reserve(1, "f.txt").await.unwrap();
        tokio::fs::write(&reservation.path, vec![0u8; 60])
            .await
            .unwrap();
        store.commit(reservation, None, 60).await;

        assert_eq!(store.total_bytes().await, 60);
        assert!(store.has_room_for(40).await);
        assert!(!store.has_room_for(41).await);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn sweeper_can_be_cancelled_cleanly() {
        let dir = temp_dir("sweeper");
        let store = std::sync::Arc::new(AttachmentStore::init(&test_config(&dir)).unwrap());
        let ct = CancellationToken::new();
        let tracker = spawn_sweeper(store, Duration::from_millis(10), ct.clone());

        tokio::time::sleep(Duration::from_millis(30)).await;
        ct.cancel();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .expect("sweeper task should exit promptly after cancellation");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
