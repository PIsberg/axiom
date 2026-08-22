//! Whether a verdict cache could be built on this dependency graph.
//!
//! The blast radius answers "what could this change break?" A cache asks the
//! other question, "what changed since the last run?", and the two are the same
//! graph read in opposite directions. If a test's forward closure is unchanged,
//! its previous verdict is still valid and neither the test nor the compilation
//! behind it has to happen again.
//!
//! Nothing here caches anything. The point is to measure whether the graph is
//! precise enough to key a cache on, before anything relies on it. The failure
//! being guarded against is specific: an under-approximated closure produces a
//! stable hash for a test whose real dependency changed, and the cache then
//! reports PASSED for code that was never run. That is the same wrong answer
//! `EvaluatorUnavailable` exists to prevent, with a longer reach, because a
//! stale green survives across sessions.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-cache-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("write the fixture file");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scan(dir: &TempDir) -> AstIndex {
    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan the fixture");
    index
}

/// The property a cache is built on: touching something a test depends on has
/// to disturb that test's key.
///
/// This is the one that must never regress. If the hash can stay still while a
/// dependency moves, every cached verdict behind it is a claim about code that
/// was not run.
#[test]
fn editing_a_dependency_changes_the_hash_of_the_test_that_uses_it() {
    let dir = TempDir::new("sound");
    // The dependency and the test live in different files on purpose. With both
    // in one file, editing the dependency also changes the test's own node hash,
    // and the assertion below would pass with the closure walk removed entirely.
    // Splitting them means only a followed edge can explain a changed key.
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 0 }\n",
    );
    dir.write(
        "gate_test.rs",
        "#[test]\nfn test_gate_opens() { assert!(is_open(1)); }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_gate_opens", AstIndex::CLOSURE_DEPTH)
        .expect("the test is indexed");
    assert!(
        closure.reachable.iter().any(|r| r.ends_with("is_open")),
        "the fixture only means something if the edge to is_open was resolved; \
         without it this test would pass on an unchanged hash for the wrong \
         reason: {closure:?}"
    );

    let before = index
        .closure_hash("test_gate_opens")
        .expect("the fixture resolves completely, so it has a key");

    // Only the dependency's body changes. The test file is not touched.
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 100 }\n",
    );

    let after = scan(&dir)
        .closure_hash("test_gate_opens")
        .expect("still resolves");

    assert_ne!(
        before, after,
        "the test calls is_open and its body changed, so a cache keyed on this \
         hash would have skipped a test that now fails"
    );
}

/// The property that makes a cache worth having: an unrelated edit must leave
/// the key alone, or nothing is ever reused and the cache is a slower way of
/// running everything.
#[test]
fn editing_an_unrelated_symbol_leaves_the_hash_alone() {
    let dir = TempDir::new("stable");
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 0 }\n\
         #[test]\n\
         fn test_gate_opens() { assert!(is_open(1)); }\n",
    );
    dir.write("unrelated.rs", "pub fn untouched_helper() -> i32 { 1 }\n");

    let before = scan(&dir)
        .closure_hash("test_gate_opens")
        .expect("has a key");

    dir.write("unrelated.rs", "pub fn untouched_helper() -> i32 { 42 }\n");

    let after = scan(&dir)
        .closure_hash("test_gate_opens")
        .expect("still has a key");

    assert_eq!(
        before, after,
        "nothing the test reaches was touched, so its verdict is still valid"
    );
}

/// An unresolved dependency name means the closure does not cover everything the
/// test reads, so there is no key to hand back.
///
/// Returning a hash anyway is the tempting bug: it would look like every other
/// key, and it would be a promise about code the graph never saw. The miss has
/// to come from refusing to produce a key, not from a caller remembering to
/// check a flag.
#[test]
fn an_unresolved_dependency_yields_no_key_at_all() {
    let dir = TempDir::new("incomplete");
    dir.write(
        "user.rs",
        "use some_external_crate::Thing;\n\
         pub fn uses_outside_world() -> bool { Thing::check() }\n\
         #[test]\n\
         fn test_reaches_outside() { assert!(uses_outside_world()); }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_reaches_outside", AstIndex::CLOSURE_DEPTH)
        .expect("the test itself is indexed");

    if closure.is_complete() {
        // Nothing left the index, so there is no incompleteness to assert on and
        // the interesting case is not being exercised. Say so rather than
        // passing quietly, which is how a branching test asserts nothing.
        panic!(
            "the fixture was supposed to reference something outside the index, \
             but every name resolved: {closure:?}"
        );
    }

    assert!(
        index.closure_hash("test_reaches_outside").is_none(),
        "an incomplete closure must not produce a key: {closure:?}"
    );
}

/// The audit reports what a cache would have decided, and reports it as counts
/// rather than as a verdict.
#[test]
fn the_audit_counts_both_directions_of_disagreement() {
    let dir = TempDir::new("audit");
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 0 }\n\
         #[test]\n\
         fn test_gate_opens() { assert!(is_open(1)); }\n",
    );
    dir.write(
        "other.rs",
        "pub fn unrelated_thing() -> i32 { 7 }\n\
         #[test]\n\
         fn test_other_thing() { assert_eq!(unrelated_thing(), 7); }\n",
    );

    let index = scan(&dir);
    let audit = index.audit_cache(1, 5);

    assert_eq!(audit.tests_in_index, 2, "{audit:?}");
    assert!(audit.symbols_audited > 0, "{audit:?}");

    // Whatever the numbers are, they have to be internally consistent: every
    // example named must be counted, and the rate must not be reported when
    // there was nothing to decide.
    assert!(
        audit.wrongly_skipped_examples.len() <= audit.would_wrongly_skip,
        "an example was named that was not counted: {audit:?}"
    );
    match audit.agreement_rate() {
        Some(rate) => assert!((0.0..=1.0).contains(&rate), "{audit:?}"),
        None => assert_eq!(
            audit.agreements + audit.would_wrongly_skip + audit.would_run_unselected,
            0,
            "a rate of None must mean there were no decisions: {audit:?}"
        ),
    }
}
