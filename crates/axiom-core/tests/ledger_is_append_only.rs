//! The ledger and the op log are append-only JSONL, and still read the old
//! whole-file array format.
//!
//! Rewriting the whole file on every append was O(n) per record and O(n^2)
//! over a session. Appending one line is O(1), at the cost that a crash
//! mid-append can leave a torn final line, which the loader skips. An array
//! written by an older axiom, or by a test fixture, still loads and is migrated
//! to lines on the next append.

use axiom_core::mcp::{append_attestation_to, load_attestations_from};
use axiom_proto::{NewAttestation, ProvenanceAttestation};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_ledger_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn record(prev: &str, prompt: &str) -> ProvenanceAttestation {
    ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "p",
        commit_merkle_root: "c",
        agent_identity: "agent",
        prompt,
        symbol_path: "s::sym",
        ctop_task_id: "t",
        verified_by: "reported",
        verification_detail: "cargo test",
        previous_seal: prev,
    })
}

#[test]
fn appends_are_lines_and_the_chain_holds() {
    let dir = tmp("append");
    let ledger = dir.join("attestations.json");

    let mut prev = String::new();
    for i in 0..5 {
        let r = record(&prev, &format!("change {i}"));
        prev = r.seal.clone();
        append_attestation_to(&ledger, &r).expect("append");
    }

    let raw = std::fs::read_to_string(&ledger).unwrap();
    assert!(
        !raw.trim_start().starts_with('['),
        "the file must be JSONL, not an array: {raw}"
    );
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 5, "one line per record");
    for line in &lines {
        serde_json::from_str::<ProvenanceAttestation>(line).expect("each line is one record");
    }

    let loaded = load_attestations_from(&ledger).expect("load");
    assert_eq!(loaded.len(), 5);
    assert!(axiom_proto::verify_chain(&loaded).is_ok(), "chain intact");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_old_array_file_still_loads_and_migrates_on_append() {
    let dir = tmp("migrate");
    let ledger = dir.join("attestations.json");

    // What an older axiom, or a test fixture, wrote: a pretty-printed array.
    let first = record("", "the first change");
    std::fs::write(
        &ledger,
        serde_json::to_string_pretty(&vec![first.clone()]).unwrap(),
    )
    .unwrap();

    let loaded = load_attestations_from(&ledger).expect("array still loads");
    assert_eq!(loaded.len(), 1);

    // Appending chains onto it and migrates the file to lines.
    let second = record(&first.seal, "the second change");
    append_attestation_to(&ledger, &second).expect("append onto an array file");

    let raw = std::fs::read_to_string(&ledger).unwrap();
    assert!(
        !raw.trim_start().starts_with('['),
        "the array must have been migrated to lines: {raw}"
    );
    let loaded = load_attestations_from(&ledger).expect("load after migrate");
    assert_eq!(loaded.len(), 2);
    assert!(axiom_proto::verify_chain(&loaded).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_torn_final_line_is_skipped_not_fatal() {
    let dir = tmp("torn");
    let ledger = dir.join("attestations.json");

    let r = record("", "a complete change");
    append_attestation_to(&ledger, &r).expect("append");

    // A crash mid-append leaves a partial line with no newline.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    f.write_all(b"{\"partial\": \"record with no closing").unwrap();
    drop(f);

    let loaded = load_attestations_from(&ledger).expect("load past the torn line");
    assert_eq!(loaded.len(), 1, "the whole record survives; the torn line is dropped");

    let _ = std::fs::remove_dir_all(&dir);
}
