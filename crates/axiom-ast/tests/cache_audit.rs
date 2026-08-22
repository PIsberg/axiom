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

use axiom_ast::{AstIndex, EnvironmentKey};
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
        .closure_hash("test_gate_opens", &EnvironmentKey::uncovered())
        .expect("the fixture resolves completely, so it has a key");

    // Only the dependency's body changes. The test file is not touched.
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 100 }\n",
    );

    let after = scan(&dir)
        .closure_hash("test_gate_opens", &EnvironmentKey::uncovered())
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
        .closure_hash("test_gate_opens", &EnvironmentKey::uncovered())
        .expect("has a key");

    dir.write("unrelated.rs", "pub fn untouched_helper() -> i32 { 42 }\n");

    let after = scan(&dir)
        .closure_hash("test_gate_opens", &EnvironmentKey::uncovered())
        .expect("still has a key");

    assert_eq!(
        before, after,
        "nothing the test reaches was touched, so its verdict is still valid"
    );
}

/// An unresolved dependency name means the closure does not cover everything the
/// An ambiguous name blocks the key outright.
///
/// This is the kind of unresolved name that is a genuine gap: something in this
/// tree satisfies the dependency and the graph cannot say which, so nothing
/// covers a change behind it. Returning a hash anyway is the tempting bug. It
/// would look like every other key and would be a promise about code the graph
/// never identified, so the miss has to come from refusing to produce a key
/// rather than from a caller remembering to check a flag.
#[test]
fn an_ambiguous_name_yields_no_key_at_all() {
    let dir = TempDir::new("ambiguous");
    // Two definitions of `shared_helper`, so a reference to it picks neither.
    dir.write("one.rs", "pub fn shared_helper() -> i32 { 1 }\n");
    dir.write("two.rs", "pub fn shared_helper() -> i32 { 2 }\n");
    dir.write(
        "user_test.rs",
        "#[test]\nfn test_uses_ambiguous() { assert_eq!(shared_helper(), 1); }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_uses_ambiguous", AstIndex::CLOSURE_DEPTH)
        .expect("the test is indexed");

    assert!(
        !closure.ambiguous.is_empty(),
        "the fixture defines shared_helper twice and only means something if that \
         name came back ambiguous: {closure:?}"
    );
    assert!(!closure.is_complete(), "{closure:?}");
    assert!(
        index
            .closure_hash("test_uses_ambiguous", &EnvironmentKey::uncovered())
            .is_none(),
        "an ambiguous name must leave the test unkeyable: {closure:?}"
    );
}

/// A name from outside the tree does not block a key, and still changes it.
///
/// This is what #17 changed. Before it, an out-of-tree name counted the same as
/// an ambiguous one, so every test importing anything was unkeyable, which was
/// every test: the audit reported 0 usable keys out of 52. The index was never
/// going to hold `std` or `anyhow`, and what those names mean is pinned by the
/// environment rather than by this tree.
///
/// Two things have to hold for that to be safe. Which outside names a test
/// reaches is an input, so adding an import must move the key. And what sits
/// behind them is the environment's business, so a different environment must
/// move it too.
#[test]
fn an_out_of_tree_name_is_covered_rather_than_treated_as_a_gap() {
    let dir = TempDir::new("outside");
    dir.write(
        "user.rs",
        "use some_external_crate::Thing;\npub fn uses_outside_world() -> bool { true }\n",
    );
    dir.write(
        "user_test.rs",
        "#[test]\nfn test_reaches_outside() { assert!(uses_outside_world()); }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("uses_outside_world", AstIndex::CLOSURE_DEPTH)
        .expect("the symbol is indexed");

    assert!(
        !closure.outside.is_empty(),
        "the fixture imports a crate that is not in the index and only means \
         something if that name landed in `outside`: {closure:?}"
    );
    assert!(
        closure.ambiguous.is_empty(),
        "nothing here is ambiguous, so nothing should block the key: {closure:?}"
    );

    let uncovered = EnvironmentKey::uncovered();
    let keyed = index
        .closure_hash("uses_outside_world", &uncovered)
        .expect("an out-of-tree name must not block a key");

    // A different environment is a different verdict. Without this, folding
    // out-of-tree names in would be a hole rather than a coverage decision: a
    // dependency upgrade would leave every cached verdict standing.
    let upgraded = EnvironmentKey::of(dir.path(), &["rustc=some-other-version".to_string()]);
    let rekeyed = index
        .closure_hash("uses_outside_world", &upgraded)
        .expect("still keyable");

    assert_ne!(
        keyed, rekeyed,
        "the environment is what covers these names, so changing it has to \
         invalidate every verdict that rested on it"
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
