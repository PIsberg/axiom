//! The artifact cache reuses compiled artifacts, never verdicts.
//!
//! The property the whole feature hangs on is that a cache hit still runs the
//! code: a failing snippet must fail again from a hit, because a cache that
//! replays verdicts is the assertion-substring fallback wearing a new hat, a
//! verdict produced by something that is not a run of the code. The second
//! property is integrity: an entry whose bytes no longer match the digests
//! recorded at store time must read as a miss and be recompiled, never
//! executed.
//!
//! Every snippet carries a per-run salt, so the first evaluation of each test
//! is a genuine miss even against a cache directory that survives across
//! runs, and the tests cannot collide with each other however they are
//! interleaved.

use axiom_proto::CtopStatus;
use axiom_vmm::{SandboxEngine, WasiEngine, artifact_cache, native};
use std::process::{Command, Stdio};

fn rustc_is_installed() -> bool {
    let installed = Command::new("rustc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    require_or_skip(installed, "rustc")
}

/// Under `AXIOM_REQUIRE_TOOLCHAINS`, which CI sets, a missing toolchain is a
/// failure rather than a skip: every test in this file returns early without
/// one, and both branches of a toolchain-conditional test are green, so the
/// suite would say nothing about whether the cache ever ran.
fn require_or_skip(available: bool, toolchain: &str) -> bool {
    if !available && std::env::var_os("AXIOM_REQUIRE_TOOLCHAINS").is_some() {
        panic!("AXIOM_REQUIRE_TOOLCHAINS is set and {toolchain} is not usable here");
    }
    available
}

/// Content no earlier run and no sibling test can have evaluated.
fn salt() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

async fn eval(snippet: &str) -> axiom_proto::CtopReport {
    WasiEngine::new()
        .expect("engine")
        .execute_eval_in("cache::probe", snippet, None)
        .await
        .expect("a report")
}

#[tokio::test]
async fn a_second_identical_snippet_skips_the_compile_and_still_passes() {
    if !rustc_is_installed() {
        return;
    }
    let snippet = format!(
        "fn main() {{ let salt = \"{}\"; assert!(salt.len() > 3); }}",
        salt()
    );

    let first = eval(&snippet).await;
    assert_eq!(first.status, CtopStatus::Passed, "{first:?}");
    assert_eq!(
        first.compile_cache.as_deref(),
        Some("miss"),
        "salted content has never been compiled before: {first:?}"
    );

    let second = eval(&snippet).await;
    assert_eq!(second.status, CtopStatus::Passed, "{second:?}");
    assert_eq!(
        second.compile_cache.as_deref(),
        Some("hit"),
        "byte-identical source under the same rustc must reuse the artifact: {second:?}"
    );
}

#[tokio::test]
async fn a_failing_snippet_fails_again_from_a_hit_rather_than_replaying() {
    if !rustc_is_installed() {
        return;
    }
    let snippet = format!(
        "fn main() {{ let salt = \"{}\"; assert!(salt.is_empty(), \"meant to fail\"); }}",
        salt()
    );

    let first = eval(&snippet).await;
    assert_eq!(first.status, CtopStatus::Failed, "{first:?}");
    assert_eq!(first.compile_cache.as_deref(), Some("miss"), "{first:?}");

    let second = eval(&snippet).await;
    assert_eq!(second.compile_cache.as_deref(), Some("hit"), "{second:?}");
    assert_eq!(
        second.status,
        CtopStatus::Failed,
        "a hit skips the compile and nothing else; the artifact runs and the \
         assertion fails again. A pass here would mean a verdict was replayed: {second:?}"
    );
}

#[tokio::test]
async fn a_one_byte_change_misses_even_when_a_neighbour_entry_exists() {
    if !rustc_is_installed() {
        return;
    }
    let a = format!(
        "fn main() {{ let salt = \"{}\"; assert_eq!(1 + 1, 2); }}",
        salt()
    );
    let b = a.replace("1 + 1", "2 + 0");

    let _ = eval(&a).await;
    let warmed = eval(&a).await;
    assert_eq!(warmed.compile_cache.as_deref(), Some("hit"), "{warmed:?}");

    let changed = eval(&b).await;
    assert_eq!(
        changed.compile_cache.as_deref(),
        Some("miss"),
        "different bytes are a different key, however close: {changed:?}"
    );
    assert_eq!(changed.status, CtopStatus::Passed, "{changed:?}");
}

#[tokio::test]
async fn a_tampered_entry_is_recompiled_never_executed() {
    if !rustc_is_installed() {
        return;
    }
    let snippet = format!(
        "fn main() {{ let salt = \"{}\"; assert!(salt.len() > 3); }}",
        salt()
    );

    let first = eval(&snippet).await;
    assert_eq!(first.compile_cache.as_deref(), Some("miss"), "{first:?}");

    // The snippet is self-contained (`fn main` is present), so what was
    // written to disk is the snippet itself and the key is derivable.
    let entry = artifact_cache::entry_dir(&artifact_cache::rustc_key(&snippet));
    let files = entry.join("files");
    assert!(
        files.is_dir(),
        "a successful compile must store its entry, expected {}",
        entry.display()
    );
    overwrite_every_file(&files);

    let second = eval(&snippet).await;
    assert_eq!(
        second.compile_cache.as_deref(),
        Some("miss"),
        "bytes that no longer match the stored digests must read as a miss: {second:?}"
    );
    assert_eq!(
        second.status,
        CtopStatus::Passed,
        "the recompile answers, and the tampered bytes never run: {second:?}"
    );
}

fn overwrite_every_file(dir: &std::path::Path) {
    for item in std::fs::read_dir(dir).expect("read the entry").flatten() {
        let path = item.path();
        if path.is_dir() {
            overwrite_every_file(&path);
        } else {
            std::fs::write(&path, b"tampered").expect("overwrite a stored artifact");
        }
    }
}

/// An empty compiler fingerprint is a key an upgrade never moves, which is
/// the `node=` failure `toolchain_fingerprints.rs` pins for the environment
/// key; this pins the same property for the artifact cache's key.
#[test]
fn the_key_covers_a_real_compiler_version() {
    if !rustc_is_installed() {
        return;
    }
    let fingerprint = artifact_cache::rustc_fingerprint();
    assert!(
        fingerprint.contains("rustc"),
        "the fingerprint must name the compiler, got {fingerprint:?}"
    );
    assert_ne!(fingerprint, "<reported no version>");
    assert_ne!(
        artifact_cache::key_of(&["probe", &fingerprint]),
        artifact_cache::key_of(&["probe", "a different toolchain"]),
        "a version change must move the key"
    );
}

/// The native tier caches through the same store: a language with a build
/// step (javac here) hits on the second identical snippet. Skipped without a
/// JDK; the rustc-tier tests above carry the feature on machines without one.
#[tokio::test]
async fn a_java_compile_is_cached_when_javac_is_available() {
    let Some(lang) = native::language_for("java") else {
        return;
    };
    if !require_or_skip(native::usable_toolchain(lang).is_some(), "javac") {
        return;
    }
    let snippet = format!("String salt = \"{}\";\nassert !salt.isEmpty();", salt());

    let engine = WasiEngine::new().expect("engine");
    let first = engine
        .execute_eval_in("Acme.java::check", &snippet, Some("java"))
        .await
        .expect("a report");
    assert_eq!(first.status, CtopStatus::Passed, "{first:?}");
    assert_eq!(first.compile_cache.as_deref(), Some("miss"), "{first:?}");

    let second = engine
        .execute_eval_in("Acme.java::check", &snippet, Some("java"))
        .await
        .expect("a report");
    assert_eq!(second.status, CtopStatus::Passed, "{second:?}");
    assert_eq!(
        second.compile_cache.as_deref(),
        Some("hit"),
        "javac's class files must be reused for identical source: {second:?}"
    );
}

#[test]
fn length_prefix_prevents_key_collisions() {
    let key1 = artifact_cache::key_of(&["ab", "c"]);
    let key2 = artifact_cache::key_of(&["a", "bc"]);
    assert_ne!(
        key1, key2,
        "length prefixing must prevent ambiguous part boundaries"
    );
}

#[tokio::test]
async fn a_corrupted_manifest_is_purged_and_recompiled() {
    if !rustc_is_installed() {
        return;
    }
    let snippet = format!(
        "fn main() {{ let salt = \"{}\"; assert!(salt.len() > 3); }}",
        salt()
    );

    let first = eval(&snippet).await;
    assert_eq!(first.compile_cache.as_deref(), Some("miss"), "{first:?}");

    let entry = artifact_cache::entry_dir(&artifact_cache::rustc_key(&snippet));
    let manifest = entry.join("manifest.json");
    assert!(manifest.is_file(), "manifest must exist");
    std::fs::write(&manifest, b"invalid json content").expect("corrupt manifest");

    let second = eval(&snippet).await;
    assert_eq!(
        second.compile_cache.as_deref(),
        Some("miss"),
        "corrupted manifest must fail restore, causing a recompile miss: {second:?}"
    );
    assert_eq!(second.status, CtopStatus::Passed, "{second:?}");
}

#[tokio::test]
async fn a_manifest_with_directory_traversal_is_purged() {
    if !rustc_is_installed() {
        return;
    }
    let snippet = format!(
        "fn main() {{ let salt = \"{}\"; assert!(salt.len() > 3); }}",
        salt()
    );

    let first = eval(&snippet).await;
    assert_eq!(first.compile_cache.as_deref(), Some("miss"), "{first:?}");

    let entry = artifact_cache::entry_dir(&artifact_cache::rustc_key(&snippet));
    let manifest = entry.join("manifest.json");
    assert!(manifest.is_file(), "manifest must exist");

    let malicious = vec![("../escaped.exe".to_string(), "anyhash".to_string())];
    std::fs::write(&manifest, serde_json::to_string(&malicious).expect("json"))
        .expect("write malicious manifest");

    let second = eval(&snippet).await;
    assert_eq!(
        second.compile_cache.as_deref(),
        Some("miss"),
        "traversal path in manifest must trigger purge and recompile: {second:?}"
    );
    assert_eq!(second.status, CtopStatus::Passed, "{second:?}");
}

#[tokio::test]
async fn non_compiled_languages_do_not_report_compile_cache() {
    let Some(lang) = native::language_for("py") else {
        return;
    };
    if !require_or_skip(native::usable_toolchain(lang).is_some(), "python") {
        return;
    }
    let snippet = format!("salt = \"{}\"\nassert len(salt) > 3", salt());
    let engine = WasiEngine::new().expect("engine");
    let report = engine
        .execute_eval_in("test.py::check", &snippet, Some("py"))
        .await
        .expect("report");
    assert_eq!(report.status, CtopStatus::Passed, "{report:?}");
    assert_eq!(
        report.compile_cache, None,
        "an interpreted language without a compile step must have no compile_cache field"
    );
}
