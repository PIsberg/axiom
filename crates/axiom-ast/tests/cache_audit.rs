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
/// An ambiguous name is over-approximated, not guessed.
///
/// The direction of the guess is the whole argument. For the blast radius, a
/// wrong extra edge costs one unnecessary test run. For a cache key, a missing
/// edge means a test is skipped on the strength of a key that did not cover what
/// changed, and a stale pass is reported for code that never ran. The two
/// mechanisms read the same graph and want opposite biases from it.
///
/// So every candidate is taken. Picking the nearest one, by file or directory,
/// was the obvious alternative and is unsafe for exactly this reason: a wrong
/// pick produces a key that looks complete and omits the dependency that moved.
#[test]
fn an_ambiguous_name_takes_every_candidate_rather_than_choosing_one() {
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

    let (name, candidates) = closure
        .over_approximated
        .iter()
        .find(|(n, _)| n.contains("shared_helper"))
        .expect(
            "the fixture defines shared_helper twice and only means something if \
             that name came back ambiguous",
        );
    assert_eq!(
        *candidates, 2,
        "both definitions of {name} must be taken, not one of them chosen"
    );
    assert!(!closure.is_precise(), "{closure:?}");

    // Both definitions are in the closure, so editing either one moves the key.
    // That is what makes over-approximation safe, and it is the property that
    // choosing the nearest candidate would break.
    let reached: Vec<&String> = closure
        .reachable
        .iter()
        .filter(|r| r.contains("shared_helper"))
        .collect();
    assert_eq!(
        reached.len(),
        2,
        "both definitions must be reachable: {closure:?}"
    );
    assert!(
        reached.iter().any(|r| r.contains("one.rs"))
            && reached.iter().any(|r| r.contains("two.rs")),
        "the closure must span both files, or one of them was silently preferred: {reached:?}"
    );

    // And it is still keyable. Ambiguity costs precision, not the key.
    let environment = EnvironmentKey::of(dir.path(), &["rustc=test".to_string()]);
    let before = index
        .closure_hash("test_uses_ambiguous", &environment)
        .expect("over-approximation must not block a key");

    // Editing the definition the test does not obviously mean must still move
    // the key, because nothing established which one it meant.
    dir.write("two.rs", "pub fn shared_helper() -> i32 { 22 }\n");
    let after = scan(&dir)
        .closure_hash("test_uses_ambiguous", &environment)
        .expect("still keyable");

    assert_ne!(
        before, after,
        "editing either candidate has to move the key; if it does not, a cache \
         would skip a test whose dependency changed"
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
        closure.is_precise(),
        "nothing here is ambiguous: {closure:?}"
    );

    // An out-of-tree name is safe only while something pins what it means. With
    // nothing covering the environment, nothing does, so the key is refused: it
    // would otherwise stay stable across the dependency upgrade that changed the
    // answer. This is the one thing that still refuses a key.
    assert!(
        index
            .closure_hash("uses_outside_world", &EnvironmentKey::uncovered())
            .is_none(),
        "a name from outside the tree must not be keyed against an environment          that covers nothing: {closure:?}"
    );

    let environment = EnvironmentKey::of(dir.path(), &["rustc=some-version".to_string()]);
    let keyed = index
        .closure_hash("uses_outside_world", &environment)
        .expect("a covered environment makes an out-of-tree name keyable");

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
    let audit = index.audit_cache(&EnvironmentKey::uncovered(), 1, 5);

    assert_eq!(audit.tests_in_index, 2, "{audit:?}");
    assert!(audit.symbols_audited > 0, "{audit:?}");

    // Whatever the numbers are, they have to be internally consistent: every
    // example named must be counted, and the rate must not be reported when
    // there was nothing to decide.
    assert!(
        audit.tests_with_a_key <= audit.tests_in_index,
        "more keys than tests: {audit:?}"
    );
    assert!(
        audit.tests_with_precise_closure <= audit.tests_in_index,
        "more precise closures than tests: {audit:?}"
    );
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

/// A method depends on the type that encloses it.
///
/// Containment is not a call, so nothing recorded it as an edge and the closure
/// of a test method did not contain its own class. The blast radius does select
/// that test when the class changes, correctly, so the two disagreed and a cache
/// keyed on the closure would have skipped a test whose class had moved.
///
/// Found by running the audit on a second tree. It did not appear on the
/// repository the audit was written in, which is the argument for running it
/// somewhere else before trusting a zero.
#[test]
fn a_method_closure_contains_the_type_that_encloses_it() {
    let dir = TempDir::new("containment");
    dir.write(
        "billing.py",
        "def compute_total(items):\n\
         \x20   return sum(items)\n\
         \n\
         class BillingTest:\n\
         \x20   def test_total(self):\n\
         \x20       assert compute_total([1, 2]) == 3\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_total", AstIndex::CLOSURE_DEPTH)
        .expect("the test method is indexed");

    assert!(
        closure
            .reachable
            .iter()
            .any(|r| r.ends_with("::BillingTest")),
        "the enclosing class must be in the closure, or editing it leaves the \
         test's key unmoved: {closure:?}"
    );
}

/// A `crate::` path names something in this tree, so failing to match one must
/// not be read as a crate from outside.
///
/// `crate::auth::validate_token` was filed as outside and folded into the
/// environment key. An in-tree dependency covered by the environment is the
/// unsound direction: editing it would not move the key. It was harmless in the
/// fixture that found it only because the same test also called the function by
/// its bare name, and nothing guarantees that.
#[test]
fn an_in_crate_path_resolves_here_rather_than_counting_as_outside() {
    let dir = TempDir::new("cratepath");
    dir.write(
        "auth.rs",
        "pub fn validate_token(token: &str) -> bool { token.len() > 10 }\n",
    );
    // The `use` line is the only mention: no bare call to fall back on, which is
    // the case that was silently unsound.
    dir.write(
        "auth_test.rs",
        "use crate::auth::validate_token;\n\
         \n\
         #[test]\n\
         fn test_uses_only_the_path() {\n\
         \x20   assert!(true);\n\
         }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_uses_only_the_path", AstIndex::CLOSURE_DEPTH)
        .expect("the test is indexed");

    assert!(
        !closure
            .outside
            .iter()
            .any(|o| o.contains("crate::auth::validate_token")),
        "a crate:: path is in this tree and must not be charged to the \
         environment: {closure:?}"
    );
    assert!(
        closure
            .reachable
            .iter()
            .any(|r| r.ends_with("::validate_token")),
        "it must resolve to the symbol it names: {closure:?}"
    );
}

/// The fallback must not reach further than it should.
///
/// Matching any dotted or colonned name by its last segment would let
/// `anyhow::Result` bind to a local `Result` and stop being covered by the
/// environment key, which is the same unsoundness pointing the other way. Only
/// `crate::`, `self::` and `super::` say "in this tree".
#[test]
fn an_external_path_is_still_charged_to_the_environment() {
    let dir = TempDir::new("external");
    dir.write("result.rs", "pub struct Result {}\n");
    dir.write(
        "user.rs",
        "use anyhow::Result;\n\
         \n\
         #[test]\n\
         fn test_uses_anyhow() {\n\
         \x20   assert!(true);\n\
         }\n",
    );

    let index = scan(&dir);
    let closure = index
        .forward_closure("test_uses_anyhow", AstIndex::CLOSURE_DEPTH)
        .expect("the test is indexed");

    assert!(
        closure.outside.iter().any(|o| o.contains("anyhow")),
        "a crate outside the tree must stay outside even when a local symbol \
         shares its last segment: {closure:?}"
    );
}

/// Behind the selector, a cache saves exactly what it unsafely skips.
///
/// Layered on blast-radius selection, a test runs when the selector picks it and
/// its key moved. So the work the cache removes is the pairs where the selector
/// picks it and the key did not: `would_wrongly_skip`, the same number the
/// safety argument turns on.
///
/// The consequence is worth stating as a test rather than as a comment, because
/// it decides whether the feature is worth building. On this design a cache
/// behind the selector cannot save anything without disagreeing with it, and
/// every disagreement is a test the selector says must run. A zero is both
/// "provably safe" and "provably pointless", and the two cannot be separated by
/// making the graph more precise.
#[test]
fn behind_the_selector_saving_and_unsafety_are_the_same_number() {
    let dir = TempDir::new("utility");
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 0 }\n",
    );
    dir.write(
        "gate_test.rs",
        "#[test]\nfn test_gate_opens() { assert!(is_open(1)); }\n",
    );
    dir.write(
        "other_test.rs",
        "#[test]\nfn test_unrelated() { assert!(true); }\n",
    );

    let index = scan(&dir);
    let audit = index.audit_cache(&EnvironmentKey::uncovered(), 1, 5);

    assert_eq!(
        audit.tests_saved_behind_the_selector(),
        audit.would_wrongly_skip,
        "these are one quantity; if they ever differ, the reasoning in the doc \
         about why the cache cannot pay for itself behind the selector is wrong \
         and must be redone: {audit:?}"
    );
}

/// The cache's own answer has to be reported, or only its safety is visible.
///
/// A run driven by the cache alone is the case selection cannot serve: a merge
/// or a pull, where nothing names the change as a symbol. That number is the
/// only argument for building the thing, so it must not be inferable-in-theory
/// and absent-in-practice.
#[test]
fn the_audit_reports_what_a_cache_driven_run_would_cost() {
    let dir = TempDir::new("cost");
    dir.write(
        "gate.rs",
        "pub fn is_open(depth: i32) -> bool { depth > 0 }\n",
    );
    dir.write(
        "gate_test.rs",
        "#[test]\nfn test_gate_opens() { assert!(is_open(1)); }\n",
    );

    let index = scan(&dir);
    let audit = index.audit_cache(&EnvironmentKey::uncovered(), 1, 5);

    let cached = audit
        .mean_tests_per_cache_run()
        .expect("symbols were audited, so there is an answer");
    let selected = audit.mean_tests_per_selected_run().expect("same");

    assert!(
        cached >= selected,
        "a cache that ran fewer tests than the selector would be skipping some \
         the selector demands, which is the unsound direction: {audit:?}"
    );
    assert!(
        cached <= audit.tests_in_index as f64,
        "a run cannot contain more tests than exist: {audit:?}"
    );
}
