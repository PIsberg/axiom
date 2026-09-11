# axiom-proto

Wire types only: `AstNode`, `CtopReport`, `ProvenanceAttestation`. The leaf of the dependency
graph; nothing here may depend on another axiom crate.

## The seal covers every stored field, and it did not always

`seal_over` hashes the two roots, the agent identity, the symbol, the task id, `verified_by`,
`verification_detail`, the timestamp and the previous seal, each length-prefixed, plus the prompt.

It once covered the roots, the identity, the prompt, the symbol and the task id only, so editing
`verified_by` from `reported` to `sandbox` in an unsigned ledger left a record that still printed
VALID, which forges the whole distinction the record exists to carry. `generate` and `verify` both
go through `seal_over` so they cannot drift, and `tests/seal_covers_the_record.rs` pins one
edited-field-fails case per field.

**Any new stored field on `ProvenanceAttestation` has to be covered, or it is forgeable.** There
are two ways to cover one, and which applies depends on the field.

A field the record carries independently goes into `seal_over`. A field that is a pure function of
fields the seal already covers is re-derived by `verify` instead, which costs nothing and does not
change the seal format. `prompt_digest` and `sandbox_trace_hash` are the second kind: the first is
a digest of the prompt, the second of `verified_by`, `verification_detail` and `ctop_proof_hash`,
all of which are sealed.

Both were outside the seal and unchecked until 2026-09-11 (#80), so editing either in a ledger left
a record that still printed VALID while naming a verification that never happened. `prompt_digest`
is worse than it looks because it is published: it is exported as
`externalParameters.promptDigest` in the SLSA statement, so an edited one goes out as though the
seal vouched for it. Adding them to `seal_over` would have invalidated every record ever issued;
re-deriving them does not.

## A seal that fails to re-derive does not say why

The seal is recomputed from a record's stored fields together with the symbol and prompt being
claimed, so it fails both when the prompt is not the one the record was issued for and when a
stored field has been edited since. There is no prompt-independent copy to compare against,
because the prompt is not stored, only a digest covering it.

So the no-match path names both causes rather than picking one. It used to report "none for this
prompt", which sent anyone holding an altered ledger looking for a typo. A broken chain is the one
piece of evidence that does point at tampering, and it is reported there too.
