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

**Any new stored field on `ProvenanceAttestation` has to be added to `seal_over`, or it is
forgeable.** `prompt_digest` and `sandbox_trace_hash` are real digests of the prompt and of the
verification, not slices of the combined digest.

## A seal that fails to re-derive does not say why

The seal is recomputed from a record's stored fields together with the symbol and prompt being
claimed, so it fails both when the prompt is not the one the record was issued for and when a
stored field has been edited since. There is no prompt-independent copy to compare against,
because the prompt is not stored, only a digest covering it.

So the no-match path names both causes rather than picking one. It used to report "none for this
prompt", which sent anyone holding an altered ledger looking for a typo. A broken chain is the one
piece of evidence that does point at tampering, and it is reported there too.
