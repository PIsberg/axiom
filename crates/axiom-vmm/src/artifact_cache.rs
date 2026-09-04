//! Content-addressed reuse of compiled evaluation artifacts.
//!
//! Compiling is most of the cost of an evaluation: `rustc` spends around
//! 200 ms on a snippet whose execution takes around 10 ms, and `javac` and
//! `kotlinc` cost more. When byte-identical source meets a byte-identical
//! toolchain the compiler's output is the same, so the build outputs are
//! stored under a key covering everything that could change them: the wrapped
//! source, the shape of the build command, the toolchain's reported version,
//! and the platform. Anything that key does not cover must not affect the
//! artifact; the temp directory's name is normalised out for exactly that
//! reason.
//!
//! What is cached is the artifact, never the verdict. On a hit the artifact
//! still runs, so a failing snippet fails again from a hit and a
//! nondeterministic one can still change its answer;
//! `crates/axiom-vmm/tests/artifact_cache.rs` pins the failing case. Every
//! stored file's BLAKE3 digest is recorded in the entry's manifest and
//! re-checked before reuse, so a tampered or truncated entry reads as a miss
//! and the snippet is recompiled.
//!
//! A cache failure must never fail an evaluation. Every error path here
//! degrades to a miss, or to quietly not storing, because the compiler can
//! always answer what the cache cannot.
//!
//! `AXIOM_EVAL_CACHE=off` disables the cache, `AXIOM_EVAL_CACHE_DIR` moves it
//! (default: `axiom-eval-cache` under the system temp directory), and
//! `AXIOM_EVAL_CACHE_MAX_MB` caps its size (default 512), enforced
//! least-recently-used after each store.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Bumped when the entry layout or key composition changes, so an old cache
/// can never be misread by a new binary.
const SCHEMA: &str = "axiom-artifact-cache-v1";

const DEFAULT_MAX_MB: u64 = 512;

pub fn enabled() -> bool {
    std::env::var("AXIOM_EVAL_CACHE")
        .map(|v| v != "off")
        .unwrap_or(true)
}

pub fn cache_root() -> PathBuf {
    if let Ok(dir) = std::env::var("AXIOM_EVAL_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::temp_dir().join("axiom-eval-cache")
}

/// A BLAKE3 hex digest over length-prefixed parts. Length-prefixed so that
/// ["ab", "c"] and ["a", "bc"] cannot collide.
pub fn key_of(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SCHEMA.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// The version string rustc reports, memoized per process.
///
/// Public so a test can pin that the key really covers the compiler: an empty
/// fingerprint here would be the `node=` failure again, a key an upgrade
/// never moves.
pub fn rustc_fingerprint() -> String {
    crate::native::program_version("rustc", &["--version"])
}

/// The key the rustc tier uses for `source` exactly as it is written to disk.
///
/// Lives here rather than beside the compile call so a test can locate the
/// entry a snippet produced without re-deriving the composition.
pub fn rustc_key(source: &str) -> String {
    key_of(&[
        "rustc",
        "-o <bin> --crate-type bin",
        &rustc_fingerprint(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        source,
    ])
}

/// Where the entry for `key` lives. Public for the tamper test.
pub fn entry_dir(key: &str) -> PathBuf {
    cache_root().join(key)
}

fn manifest_path(entry: &Path) -> PathBuf {
    entry.join("manifest.json")
}

fn purge(entry: &Path) {
    let _ = std::fs::remove_dir_all(entry);
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Copy the entry for `key` into `work_dir`, or report a miss.
///
/// Every file is read and its digest checked against the manifest *before*
/// anything is written, so a bad entry is purged without leaving half a
/// restore behind. Returns false on any doubt; the caller then compiles as if
/// the cache did not exist.
pub fn restore(key: &str, work_dir: &Path) -> bool {
    if !enabled() {
        return false;
    }
    let entry = entry_dir(key);
    let Ok(text) = std::fs::read_to_string(manifest_path(&entry)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Vec<(String, String)>>(&text) else {
        purge(&entry);
        return false;
    };
    if manifest.is_empty() {
        purge(&entry);
        return false;
    }

    // Pass 1: verify everything.
    let mut verified: Vec<(&String, Vec<u8>, std::fs::Permissions)> = Vec::new();
    for (rel, want) in &manifest {
        // A manifest is data on disk, not something this process wrote, so a
        // path that could step outside the work directory is refused.
        if rel.contains("..") || Path::new(rel).is_absolute() {
            purge(&entry);
            return false;
        }
        let cached = entry.join("files").join(rel);
        let Ok(bytes) = std::fs::read(&cached) else {
            purge(&entry);
            return false;
        };
        if blake3::hash(&bytes).to_hex().to_string() != *want {
            purge(&entry);
            return false;
        }
        let Ok(meta) = std::fs::metadata(&cached) else {
            purge(&entry);
            return false;
        };
        verified.push((rel, bytes, meta.permissions()));
    }

    // Pass 2: write the verified bytes, carrying the permissions so the
    // execute bit survives on Unix.
    for (rel, bytes, perms) in verified {
        let dest = work_dir.join(rel);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&dest, &bytes).is_err() {
            return false;
        }
        let _ = std::fs::set_permissions(&dest, perms);
    }

    // The eviction witness: least-recently-used means least-recently-restored.
    let _ = std::fs::write(entry.join("used"), unix_now().to_string());
    true
}

/// Store every file under `work_dir` except the ones named in `exclude`
/// (paths relative to `work_dir`, forward slashes), as the entry for `key`.
///
/// Best-effort on purpose: the artifact was just built and is about to run,
/// so nothing here may fail the evaluation. The entry is staged beside its
/// final name and renamed into place, so a concurrent process sees either no
/// entry or a whole one; the loser of a rename race throws its staging away.
pub fn store(key: &str, work_dir: &Path, exclude: &[&str]) {
    static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

    if !enabled() {
        return;
    }
    let root = cache_root();
    let entry = root.join(key);
    if entry.exists() {
        return;
    }
    let staging = root.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let mut manifest: Vec<(String, String)> = Vec::new();
    if copy_tree(
        work_dir,
        work_dir,
        exclude,
        &staging.join("files"),
        &mut manifest,
    )
    .is_err()
        || manifest.is_empty()
    {
        let _ = std::fs::remove_dir_all(&staging);
        return;
    }
    let json = match serde_json::to_string(&manifest) {
        Ok(j) => j,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&staging);
            return;
        }
    };
    if std::fs::write(manifest_path(&staging), json).is_err()
        || std::fs::write(staging.join("used"), unix_now().to_string()).is_err()
        || std::fs::rename(&staging, &entry).is_err()
    {
        let _ = std::fs::remove_dir_all(&staging);
        return;
    }
    evict_to_cap(&root);
}

/// Walk `dir`, copying each file into `staged` under its path relative to
/// `base` and recording the digest of what was staged, so the manifest
/// describes the stored bytes rather than the originals.
fn copy_tree(
    base: &Path,
    dir: &Path,
    exclude: &[&str],
    staged: &Path,
    manifest: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    for item in std::fs::read_dir(dir)? {
        let path = item?.path();
        if path.is_dir() {
            copy_tree(base, &path, exclude, staged, manifest)?;
            continue;
        }
        let rel = path
            .strip_prefix(base)
            .map_err(|_| std::io::Error::other("not under the work directory"))?
            .to_string_lossy()
            .replace('\\', "/");
        if exclude.contains(&rel.as_str()) {
            continue;
        }
        let dest = staged.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&path, &dest)?;
        let bytes = std::fs::read(&dest)?;
        manifest.push((rel, blake3::hash(&bytes).to_hex().to_string()));
    }
    Ok(())
}

/// Prune the cache to its size cap. Public so the daemon pool's eviction
/// pass can own cache housekeeping alongside worker eviction.
pub fn evict_cache_to_cap() {
    evict_to_cap(&cache_root());
}

/// Remove least-recently-used entries until the cache fits its cap.
fn evict_to_cap(root: &Path) {
    let cap_bytes = std::env::var("AXIOM_EVAL_CACHE_MAX_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_MB)
        .saturating_mul(1024 * 1024);

    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    // (last used, size, path) per complete entry; staging dirs are skipped.
    let mut entries: Vec<(u64, u64, PathBuf)> = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let name = item.file_name();
        if !path.is_dir() || name.to_string_lossy().starts_with(".tmp-") {
            continue;
        }
        let used = std::fs::read_to_string(path.join("used"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        entries.push((used, dir_size(&path), path));
    }
    let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
    if total <= cap_bytes {
        return;
    }
    entries.sort();
    for (_, size, path) in entries {
        if total <= cap_bytes {
            break;
        }
        purge(&path);
        total = total.saturating_sub(size);
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.flatten()
        .map(|item| {
            let path = item.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                item.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
