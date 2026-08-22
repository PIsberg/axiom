//! What the environment key is allowed to claim it covers.
//!
//! `EnvironmentKey` is what makes it safe for a closure to treat a name from
//! outside the tree as covered rather than as a gap. `anyhow::Result` will never
//! resolve to an indexed symbol, and what it means is fixed by the compiler and
//! the lock file; if either moves, every cached verdict has to move with it.
//!
//! That argument only holds while the fingerprints actually change when a
//! toolchain does. The first version of this reused `probe_args`, which are
//! chosen to produce no output: `python -c pass` and `node -e ""` are silent by
//! design. The key contained `node=` and `python=`, so upgrading either would
//! not have invalidated a single verdict, and nothing said so.

use axiom_vmm::native;

/// Every installed toolchain has to report something, or the key is not
/// covering it and the closure had no business folding its names in.
#[test]
fn an_installed_toolchain_reports_a_version() {
    let fingerprints = native::toolchain_fingerprints();

    if fingerprints.is_empty() {
        // No toolchain on this machine. Nothing to assert, and saying so beats
        // a silent pass, which is the branching-test failure #9 was about.
        eprintln!("no toolchain installed here, so nothing was fingerprinted");
        return;
    }

    for fingerprint in &fingerprints {
        let (program, version) = fingerprint.split_once('=').unwrap_or_else(|| {
            panic!("a fingerprint must be program=version, got {fingerprint:?}")
        });

        assert!(
            !version.trim().is_empty(),
            "{program} is installed and reported an empty version, so the key it \
             feeds cannot change when {program} is upgraded: {fingerprints:?}"
        );
        assert_ne!(
            version, "<reported no version>",
            "{program} is installed but answered nothing to its version arguments. \
             Folding its names into the key would claim a coverage that is not \
             there: {fingerprints:?}"
        );
    }
}

/// The same environment has to produce the same key, or nothing derived from it
/// can be compared between two runs.
#[test]
fn fingerprints_are_stable_and_ordered() {
    let first = native::toolchain_fingerprints();
    let second = native::toolchain_fingerprints();
    assert_eq!(first, second, "two reads of one environment must agree");

    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(
        first, sorted,
        "the order must not depend on which language happened to probe first"
    );
}

/// On a runner that promised toolchains, every language must actually have one.
///
/// The evaluator tests all branch: with a toolchain they assert the verdict,
/// without one they assert the refusal. Both branches are green, so a runner
/// where an install step silently did nothing passes every test without running
/// any of them. That is how the TypeScript recipe reached main unrun (#9), and
/// how the Kotlin and Scala ones nearly did.
///
/// `AXIOM_REQUIRE_TOOLCHAINS` is set in CI and unset on a developer machine, so
/// this is a loud failure where it matters and silent where it would be noise.
#[test]
fn every_language_has_a_toolchain_when_the_environment_promises_one() {
    if std::env::var_os("AXIOM_REQUIRE_TOOLCHAINS").is_none() {
        eprintln!("AXIOM_REQUIRE_TOOLCHAINS is unset, so a missing toolchain is not an error here");
        return;
    }

    let missing: Vec<&str> = native::languages()
        .iter()
        .filter(|language| native::usable_toolchain(language).is_none())
        .map(|language| language.extension)
        .collect();

    assert!(
        missing.is_empty(),
        "AXIOM_REQUIRE_TOOLCHAINS is set, so every language must be runnable here, \
         but {missing:?} had none. Every test for those languages just asserted the \
         refusal branch and told you nothing about the recipe."
    );
}
