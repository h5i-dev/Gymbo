//! Where downloaded bytes live on disk.
//!
//! The layout is URL-keyed, the way coursier's cache is: a file fetched from
//! `https://repo1.maven.org/maven2/g/a/1.0/a-1.0.jar` lands at
//! `<root>/https/repo1.maven.org/maven2/g/a/1.0/a-1.0.jar`. Keying by URL rather
//! than by coordinates is what makes two repositories serving the same
//! coordinates unambiguous — a mistake that shows up as one repository's
//! artifact silently satisfying a request meant for another.
//!
//! Three sidecars sit next to each entry, all borrowed from upstream practice:
//!
//! | Sidecar | Meaning |
//! |---|---|
//! | `.part` | a download in progress; never read as content |
//! | `.error` | this URL 404'd, with the time, so a missing artifact is not re-requested from every repository on every run |
//! | `.checked` | when this URL was last confirmed fresh, for update policies |
//! | `.lock` | held while downloading, so two jv processes sharing a cache do not fetch the same file twice |

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs4::fs_std::FileExt;

/// A cache directory could not be used.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot use the cache at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{url} is not a URL jv can cache")]
    UnsupportedUrl { url: String },
}

/// Marks the on-disk format, so a future change can invalidate rather than
/// misread what an older jv wrote.
pub const FORMAT_VERSION: &str = "v1";

/// The content store.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// A store rooted at a directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store in the user's cache directory, which is where jv keeps it
    /// unless told otherwise.
    pub fn default_location() -> Option<Self> {
        dirs::cache_dir().map(|cache| Store::new(cache.join("jv").join(FORMAT_VERSION)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The cache path for a URL.
    ///
    /// # Examples
    ///
    /// ```
    /// use jv_cache::Store;
    ///
    /// let store = Store::new("/cache");
    /// let path = store
    ///     .path_for("https://repo1.maven.org/maven2/g/a/1.0/a-1.0.jar")
    ///     .unwrap();
    /// assert!(path.ends_with("https/repo1.maven.org/maven2/g/a/1.0/a-1.0.jar"));
    /// ```
    pub fn path_for(&self, url: &str) -> Result<PathBuf, StoreError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| StoreError::UnsupportedUrl {
                url: url.to_owned(),
            })?;
        let mut path = self.root.join(sanitize(scheme));
        // Query strings and fragments do not address artifacts in a Maven
        // repository, and would produce paths no filesystem accepts.
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        for segment in rest.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                continue;
            }
            path.push(sanitize(segment));
        }
        Ok(path)
    }

    /// Reads a cached entry, if it is present.
    pub fn read(&self, url: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.path_for(url)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    /// Writes an entry, atomically.
    ///
    /// The bytes go to a `.part` file first and are then renamed, so a killed
    /// process leaves either the old content or none — never a half-written jar
    /// that later reads as a corrupt artifact.
    pub fn write(&self, url: &str, bytes: &[u8]) -> Result<PathBuf, StoreError> {
        let path = self.path_for(url)?;
        self.write_at(&path, bytes, Durability::Synced)?;
        Ok(path)
    }

    fn write_at(
        &self,
        path: &Path,
        bytes: &[u8],
        durability: Durability,
    ) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        // A sidecar is one small write whose loss costs nothing — at worst an
        // update policy re-checks something. Paying for a temp file, an fsync
        // and a rename on each one doubles the syscalls of a cold resolve for no
        // safety that anybody would notice.
        if durability == Durability::Loose {
            return fs::write(path, bytes).map_err(|source| StoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }

        // Unique per writer. A shared `.part` name means two writers of one URL
        // race: the first renames it away and the second's rename fails with
        // ENOENT, which surfaces as a resolve aborting on a file that is in fact
        // present. Reachable whenever the download lock could not be taken.
        let part = with_suffix(
            path,
            &format!("part.{}.{}", std::process::id(), next_write()),
        );
        {
            let mut file = fs::File::create(&part).map_err(|source| StoreError::Io {
                path: part.clone(),
                source,
            })?;
            file.write_all(bytes).map_err(|source| StoreError::Io {
                path: part.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| StoreError::Io {
                path: part.clone(),
                source,
            })?;
        }
        fs::rename(&part, path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Records that a URL is known to be absent.
    ///
    /// Without this, a resolve that touches a repository which does not carry an
    /// artifact pays for that 404 on every run, and a multi-repository setup
    /// pays for it once per repository.
    pub fn record_missing(&self, url: &str) -> Result<(), StoreError> {
        let path = with_suffix(&self.path_for(url)?, "error");
        self.write_at(&path, timestamp().as_bytes(), Durability::Loose)
    }

    /// When a URL was last recorded as absent.
    pub fn missing_since(&self, url: &str) -> Result<Option<SystemTime>, StoreError> {
        modified_time(&with_suffix(&self.path_for(url)?, "error"))
    }

    /// Forgets that a URL was absent, which a successful fetch must do.
    pub fn clear_missing(&self, url: &str) -> Result<(), StoreError> {
        let path = with_suffix(&self.path_for(url)?, "error");
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    /// Records that a URL was just confirmed fresh.
    pub fn record_checked(&self, url: &str) -> Result<(), StoreError> {
        let path = with_suffix(&self.path_for(url)?, "checked");
        self.write_at(&path, timestamp().as_bytes(), Durability::Loose)
    }

    /// When a URL was last confirmed fresh.
    pub fn checked_at(&self, url: &str) -> Result<Option<SystemTime>, StoreError> {
        modified_time(&with_suffix(&self.path_for(url)?, "checked"))
    }

    /// Takes an exclusive lock on one URL's entry, blocking until it is free.
    ///
    /// Two jv processes sharing a cache — a CI job running several modules, or a
    /// developer with two terminals — would otherwise download the same jar
    /// twice. The write itself is already safe without this, since it goes
    /// through a rename; the lock is about not doing the work twice.
    ///
    /// The lock is released when the returned guard is dropped, and by the
    /// operating system if the process dies, so a crash cannot leave a cache
    /// that later runs block on forever.
    ///
    /// **This blocks the calling thread.** Async callers want [`Store::try_lock`]
    /// instead: `flock` inside a future would park a runtime worker, and a
    /// downloader running many fetches at once would deadlock itself.
    pub fn lock(&self, url: &str) -> Result<DownloadLock, StoreError> {
        let file = self.lock_file(url)?;
        FileExt::lock_exclusive(&file.handle).map_err(|source| StoreError::Io {
            path: file.path,
            source,
        })?;
        Ok(DownloadLock { file: file.handle })
    }

    /// Takes the lock if it is free, and reports rather than waits if it is not.
    ///
    /// The non-blocking half of [`Store::lock`], for callers inside an async
    /// runtime. `Ok(None)` means somebody else is downloading this URL right
    /// now; the caller decides whether to wait for the result to appear in the
    /// cache or to fetch it again.
    pub fn try_lock(&self, url: &str) -> Result<Locking, StoreError> {
        let file = self.lock_file(url)?;
        match FileExt::try_lock_exclusive(&file.handle) {
            Ok(true) => Ok(Locking::Held(DownloadLock { file: file.handle })),
            Ok(false) => Ok(Locking::Contended),
            // A filesystem that cannot lock at all — an NFS mount with no lockd,
            // some container overlays — must be distinguished from a contended
            // lock. Treating the two alike made every fetch wait out the full
            // "somebody else is downloading" timeout before doing the work
            // itself, turning a half-minute resolve into hours.
            Err(_) => Ok(Locking::Unavailable),
        }
    }

    /// Opens (creating if needed) the lock file for a URL.
    fn lock_file(&self, url: &str) -> Result<LockFile, StoreError> {
        let path = with_suffix(&self.path_for(url)?, "lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let handle = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(LockFile { path, handle })
    }
}

/// What [`Store::try_lock`] found.
#[derive(Debug)]
pub enum Locking {
    /// The lock is this caller's until the guard drops.
    Held(DownloadLock),
    /// Somebody else is downloading this URL. Waiting for their result is
    /// worthwhile.
    Contended,
    /// This filesystem does not support locking. Waiting would be waiting for
    /// nobody; the atomic rename makes the write safe without it.
    Unavailable,
}

/// An opened lock file, before anything has been locked.
struct LockFile {
    path: PathBuf,
    handle: fs::File,
}

/// How hard a write tries to survive a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Durability {
    /// Temp file, fsync, rename. For content, where a torn file would later be
    /// read as a corrupt artifact.
    Synced,
    /// A plain write. For sidecars, whose loss costs nothing.
    Loose,
}

/// Holds one URL's download lock until dropped.
#[derive(Debug)]
pub struct DownloadLock {
    file: fs::File,
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        // Nothing useful can be done if the release fails, and the operating
        // system releases it when the handle closes a moment later anyway.
        let _ = FileExt::unlock(&self.file);
    }
}

/// Replaces characters no filesystem should be asked to hold.
///
/// Repository URLs are tame in practice; this exists so that a hostile or
/// malformed one cannot escape the cache directory or produce an unopenable
/// name.
fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push('.');
    name.push_str(suffix);
    path.with_file_name(name)
}

fn modified_time(path: &Path) -> Result<Option<SystemTime>, StoreError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.modified().ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A counter that makes a temp file name unique within this process.
fn next_write() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Store::new(dir.path());
        (dir, store)
    }

    const JAR: &str =
        "https://repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar";

    #[test]
    fn the_path_mirrors_the_url() {
        let (_dir, store) = store();
        let path = store.path_for(JAR).unwrap();
        assert!(path.starts_with(store.root()));
        assert!(path.ends_with(
            "https/repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.9/slf4j-api-2.0.9.jar"
        ));
    }

    #[test]
    fn two_repositories_serving_one_coordinate_do_not_collide() {
        let (_dir, store) = store();
        let first = store
            .path_for("https://repo1.maven.org/maven2/g/a/1.0/a-1.0.jar")
            .unwrap();
        let second = store
            .path_for("https://nexus.corp/public/g/a/1.0/a-1.0.jar")
            .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_url_without_a_scheme_is_refused() {
        let (_dir, store) = store();
        assert!(matches!(
            store.path_for("repo1.maven.org/x"),
            Err(StoreError::UnsupportedUrl { .. })
        ));
    }

    #[test]
    fn traversal_segments_cannot_escape_the_root() {
        let (_dir, store) = store();
        let path = store.path_for("https://host/../../../etc/passwd").unwrap();
        assert!(path.starts_with(store.root()));
        assert!(path.ends_with("https/host/etc/passwd"));
    }

    #[test]
    fn query_strings_are_dropped() {
        let (_dir, store) = store();
        let plain = store.path_for("https://host/a/b.jar").unwrap();
        let with_query = store.path_for("https://host/a/b.jar?token=x").unwrap();
        assert_eq!(plain, with_query);
    }

    #[test]
    fn writing_then_reading_round_trips() {
        let (_dir, store) = store();
        assert_eq!(store.read(JAR).unwrap(), None);
        store.write(JAR, b"jar bytes").unwrap();
        assert_eq!(store.read(JAR).unwrap().as_deref(), Some(&b"jar bytes"[..]));
    }

    #[test]
    fn a_write_leaves_no_part_file_behind() {
        let (_dir, store) = store();
        let path = store.write(JAR, b"bytes").unwrap();
        assert!(path.exists());
        assert!(!with_suffix(&path, "part").exists());
    }

    #[test]
    fn a_freshness_stamp_is_recorded_only_when_asked_for() {
        let (_dir, store) = store();
        store.write(JAR, b"bytes").unwrap();
        // Writing does not stamp: most of what jv caches is immutable, and a
        // second file plus a second directory walk per artifact is a real cost
        // on a cold resolve of a few hundred of them.
        assert_eq!(store.checked_at(JAR).unwrap(), None);

        store.record_checked(JAR).unwrap();
        assert!(store.checked_at(JAR).unwrap().is_some());
    }

    #[test]
    fn absence_is_remembered_and_can_be_forgotten() {
        let (_dir, store) = store();
        assert_eq!(store.missing_since(JAR).unwrap(), None);
        store.record_missing(JAR).unwrap();
        assert!(store.missing_since(JAR).unwrap().is_some());
        store.clear_missing(JAR).unwrap();
        assert_eq!(store.missing_since(JAR).unwrap(), None);
    }

    #[test]
    fn clearing_an_absent_marker_is_not_an_error() {
        let (_dir, store) = store();
        assert!(store.clear_missing(JAR).is_ok());
    }

    #[test]
    fn a_second_lock_on_one_url_is_reported_rather_than_waited_for() {
        let (_dir, store) = store();
        let held = store.try_lock(JAR).unwrap();
        assert!(matches!(held, Locking::Held(_)));
        // Blocking here instead would park a tokio worker, and a fetcher running
        // many downloads at once would deadlock against its own prefetches.
        assert!(matches!(store.try_lock(JAR).unwrap(), Locking::Contended));
        drop(held);
        assert!(matches!(store.try_lock(JAR).unwrap(), Locking::Held(_)));
    }

    #[test]
    fn locks_on_different_urls_do_not_contend() {
        let (_dir, store) = store();
        let first = store.try_lock(JAR).unwrap();
        let second = store
            .try_lock("https://repo1.maven.org/maven2/g/a/1.0/a-1.0.jar")
            .unwrap();
        assert!(matches!(first, Locking::Held(_)));
        assert!(matches!(second, Locking::Held(_)));
    }

    #[test]
    fn two_writers_of_one_url_do_not_destroy_each_others_temp_file() {
        // Without a per-writer temp name the first writer renames it away and the
        // second's rename fails with ENOENT — a resolve aborting on a file that
        // is in fact present. Reachable whenever the lock could not be taken.
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Store::new(dir.path());
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let store = store.clone();
                    scope.spawn(move || store.write(JAR, b"bytes"))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for result in &results {
            assert!(result.is_ok(), "{result:?}");
        }
        assert_eq!(store.read(JAR).unwrap().as_deref(), Some(&b"bytes"[..]));
    }

    #[test]
    fn sidecars_sit_beside_the_entry_rather_than_replacing_it() {
        let (_dir, store) = store();
        store.write(JAR, b"bytes").unwrap();
        store.record_missing(JAR).unwrap();
        // The content survives an error marker written next to it.
        assert_eq!(store.read(JAR).unwrap().as_deref(), Some(&b"bytes"[..]));
    }
}
