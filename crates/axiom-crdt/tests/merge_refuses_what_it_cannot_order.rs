//! A merge that is about to be written into a source file has to refuse what it
//! cannot order, rather than guess.
//!
//! `merge_statements_3way` used to settle any two edits made at the same place
//! by keeping both, skipping a remote line when the local side already held an
//! identical one. On its own that produced code nobody wrote: two agents who
//! each rewrote one line got both lines, and two agents who each added a block
//! ending in `}` got one block with its brace and one without. None of it was
//! reported as a conflict. It went unnoticed while the function was only
//! exercised by tests; it is now what decides the text of a file.

use axiom_crdt::merge_statements_3way;

#[test]
fn two_rewrites_of_one_line_are_a_conflict_not_both_lines() {
    let base = "fn f() {\n    let timeout = 30;\n    run(timeout)\n}";
    let local = "fn f() {\n    let timeout = 60;\n    run(timeout)\n}";
    let remote = "fn f() {\n    let timeout = 90;\n    run(timeout)\n}";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);

    assert!(
        has_conflicts,
        "each side rewrote the same line differently, and no order of the two is right; got:\n{merged}"
    );
    assert!(merged.contains("let timeout = 60;") && merged.contains("let timeout = 90;"));
}

#[test]
fn two_blocks_added_at_one_place_keep_every_line_and_conflict() {
    let base = "impl S {\n}";
    let local = "impl S {\n    fn a() {\n    }\n}";
    let remote = "impl S {\n    fn b() {\n    }\n}";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);

    assert!(
        has_conflicts,
        "both sides inserted at the same point, so their order is a guess; got:\n{merged}"
    );
    // Each block arrives whole. Dropping the remote closing brace because the
    // local block also had one is how this used to produce unbalanced code.
    assert_eq!(
        merged.matches("    }").count(),
        2,
        "a line that both blocks contain must not be deduplicated away; got:\n{merged}"
    );
}

#[test]
fn a_rewrite_against_a_deletion_is_a_conflict() {
    let base = "a\nx\nb";
    let local = "a\nx1\nb";
    let remote = "a\nb";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);

    assert!(
        has_conflicts,
        "one side changed a line the other deleted; keeping the change silently discards the deletion; got:\n{merged}"
    );
}

#[test]
fn separate_edits_still_merge_cleanly() {
    let base = "a\nb\nc\nd\ne";
    let local = "a\nB\nc\nd\ne";
    let remote = "a\nb\nc\nD\ne";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);

    assert!(
        !has_conflicts,
        "the edits touch different lines; got:\n{merged}"
    );
    assert_eq!(merged, "a\nB\nc\nD\ne");
}

#[test]
fn the_same_edit_on_both_sides_is_taken_once() {
    let base = "a\nb\nc";
    let both = "a\nb2\nextra\nc";

    let (merged, has_conflicts) = merge_statements_3way(base, both, both);

    assert!(!has_conflicts);
    assert_eq!(merged, both);
}

#[test]
fn a_clean_merge_is_the_same_whichever_side_is_local() {
    let base = "a\nb\nc\nd\ne\nf";
    let one = "a\nb\nnew1\nc\nd\ne\nf";
    let two = "a\nb\nc\nd\ne2\nf";

    let (ab, conflict_ab) = merge_statements_3way(base, one, two);
    let (ba, conflict_ba) = merge_statements_3way(base, two, one);

    assert!(!conflict_ab && !conflict_ba);
    assert_eq!(
        ab, ba,
        "a merge whose result depends on which agent wrote first is an ordering, not a merge"
    );
    assert_eq!(ab, "a\nb\nnew1\nc\nd\ne2\nf");
}

#[test]
fn rewrites_of_adjacent_lines_merge_in_base_order() {
    // diff3 as git runs it calls this a conflict, because no unchanged line
    // separates the two edits. Nothing about the order is unknown, though: each
    // side replaced a different base line, and base says which comes first.
    let base = "a\nb\nc\nd";
    let local = "a\nB\nc\nd";
    let remote = "a\nb\nC\nd";

    let (ab, conflict_ab) = merge_statements_3way(base, local, remote);
    let (ba, conflict_ba) = merge_statements_3way(base, remote, local);

    assert!(!conflict_ab && !conflict_ba, "got:\n{ab}\n--\n{ba}");
    assert_eq!(ab, "a\nB\nC\nd");
    assert_eq!(ba, ab);
}

#[test]
fn an_insertion_touching_another_edit_is_a_conflict() {
    // One side adds a line directly after `b`; the other rewrites `b`. Whether
    // the new line belongs after the rewritten one is a guess.
    let base = "a\nb\nc";
    let local = "a\nb\nnew\nc";
    let remote = "a\nB\nc";

    let (merged, has_conflicts) = merge_statements_3way(base, local, remote);

    assert!(has_conflicts, "got:\n{merged}");
}
