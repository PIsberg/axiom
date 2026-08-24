//! Ingesting a SCIP index into the symbol graph.
//!
//! The line-based parsers are heuristics: they guess a symbol's owner and its
//! dependencies from the shape of the text, and the invariants file records the
//! long list of ways that has been wrong. A SCIP index is the opposite. It is
//! produced by a language's own indexer, `scip-java`, `rust-analyzer scip` and
//! the rest, which run the real compiler, so every definition and reference is
//! resolved rather than matched. Where one is available it is the ground truth
//! the heuristics only approximate.
//!
//! Each defined symbol becomes a node, its descriptors rendered into a readable
//! key (`com.example.Foo#bar`), its kind mapped from the SCIP `Kind`, a test
//! recognised by the `Test` role. Each reference occurrence is charged to the
//! definition it sits inside, and the edge to the referenced symbol is kept
//! when that symbol is defined somewhere in the index and dropped when it names
//! a library outside it. The resulting `dependencies`, and the blast radius
//! over them, rest on resolved edges rather than string matches a comment or a
//! same-named local could forge.

use crate::{AstIndex, ScanSummary};
use protobuf::Message;
use scip::types::Index;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// SCIP `SymbolRole` bit flags, from the protocol.
const ROLE_DEFINITION: i32 = 1;
const ROLE_TEST: i32 = 32;

/// Render a SCIP symbol string into a readable, stable axiom key.
///
/// A local symbol (`local 3`) and an unparseable one are not indexed: locals
/// are function-scoped and never referenced across a boundary. The package name
/// and version are dropped, so the key is the same across a version bump, the
/// way relative file keys are the same across machines; Java carries its package
/// boundary in the descriptors, so nothing unique is lost by it.
pub fn render_symbol(raw: &str) -> Option<String> {
    if raw.trim().is_empty() || raw.starts_with("local ") {
        return None;
    }
    let sym = scip::symbol::parse_symbol(raw).ok()?;
    use scip::types::descriptor::Suffix;
    let mut out = String::new();
    for d in &sym.descriptors {
        let name = d.name.trim();
        if name.is_empty() {
            continue;
        }
        match d.suffix.enum_value_or_default() {
            Suffix::Namespace | Suffix::Package => {
                out.push_str(name);
                out.push('.');
            }
            Suffix::Type => {
                out.push_str(name);
                out.push('#');
            }
            Suffix::Method | Suffix::Term => {
                out.push_str(name);
                if !d.disambiguator.is_empty() {
                    out.push('+');
                    out.push_str(&d.disambiguator);
                }
            }
            _ => out.push_str(name),
        }
    }
    let trimmed = out.trim_end_matches(['.', '#']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// A SCIP range is `[startLine, startChar, endChar]` on one line or
/// `[startLine, startChar, endLine, endChar]` across several, zero-based. This
/// returns the zero-based `(startLine, endLine)`.
fn line_span(range: &[i32]) -> Option<(usize, usize)> {
    match range.len() {
        3 => Some((range[0] as usize, range[0] as usize)),
        4 => Some((range[0] as usize, range[2] as usize)),
        _ => None,
    }
}

/// Does `outer` contain `inner`, by line span?
fn contains(outer: (usize, usize), inner: (usize, usize)) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// One definition read out of a document.
struct Def {
    raw: String,
    decl: (usize, usize),
    body: (usize, usize),
    /// True when SCIP gave no `enclosing_range` and the body was defaulted to
    /// the declaration line, so it may be widened to the next definition. A
    /// real single-line `enclosing_range` is left exactly as it is.
    widen: bool,
    kind: String,
    signature: String,
}

/// Map a SCIP `SymbolInformation` kind to the small vocabulary axiom uses.
fn kind_of(si: &scip::types::SymbolInformation) -> String {
    use scip::types::symbol_information::Kind;
    match si.kind.enum_value_or_default() {
        Kind::Class | Kind::Interface | Kind::Enum | Kind::Struct | Kind::Trait => {
            "type".to_string()
        }
        Kind::Method => "method".to_string(),
        Kind::Field => "field".to_string(),
        _ => "function".to_string(),
    }
}

impl AstIndex {
    /// Read a `.scip` file and index it, resolving relative paths against
    /// `project_root` (the directory the index was generated in).
    pub fn ingest_scip(
        &self,
        scip_path: &Path,
        project_root: &Path,
    ) -> std::io::Result<ScanSummary> {
        let bytes = std::fs::read(scip_path)?;
        let index = Index::parse_from_bytes(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{scip_path:?} is not a valid SCIP index: {e}"),
            )
        })?;
        self.ingest_scip_index(&index, project_root)
    }

    /// Index an already-parsed SCIP index. Split out so a test can build one in
    /// memory rather than shelling out to an indexer.
    pub fn ingest_scip_index(
        &self,
        index: &Index,
        project_root: &Path,
    ) -> std::io::Result<ScanSummary> {
        let abs_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        *self.scan_root.write().unwrap() = Some(abs_root.clone());

        // Every symbol defined anywhere in the index. A reference to one of
        // these is a real edge, cross-file included; a reference to anything
        // else names a library outside the index and is dropped.
        let mut defined_raw: HashSet<String> = HashSet::new();
        for doc in &index.documents {
            for occ in &doc.occurrences {
                if occ.symbol_roles & ROLE_DEFINITION != 0 && render_symbol(&occ.symbol).is_some() {
                    defined_raw.insert(occ.symbol.clone());
                }
            }
        }

        // Display names and kinds come from SymbolInformation, keyed by raw
        // symbol across the whole index.
        let mut info: HashMap<String, (String, String)> = HashMap::new();
        for doc in &index.documents {
            for si in &doc.symbols {
                info.insert(si.symbol.clone(), (kind_of(si), si.display_name.clone()));
            }
        }

        // Edges the indexer states outright through `relationships`: an
        // implementation of an interface, a symbol whose type is another, a
        // reference recorded on the symbol rather than as an occurrence. Each is
        // an edge from the symbol that carries the relationship to the one it
        // names, kept when that target is defined in the index. These reach what
        // occurrence edges miss: a change to an interface reaching its
        // implementors, a change to a type reaching what is typed by it.
        let mut rel_edges: HashMap<String, HashSet<String>> = HashMap::new();
        for doc in &index.documents {
            for si in &doc.symbols {
                if render_symbol(&si.symbol).is_none() {
                    continue;
                }
                for r in &si.relationships {
                    if (r.is_implementation
                        || r.is_type_definition
                        || r.is_reference
                        || r.is_definition)
                        && defined_raw.contains(&r.symbol)
                        && r.symbol != si.symbol
                    {
                        rel_edges
                            .entry(si.symbol.clone())
                            .or_default()
                            .insert(r.symbol.clone());
                    }
                }
            }
        }

        let mut files_scanned = 0usize;
        let mut nodes_indexed = 0usize;

        for doc in &index.documents {
            let rel = doc.relative_path.replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            files_scanned += 1;
            self.forget_file(&rel);

            let text_lines: Vec<&str> = if doc.text.is_empty() {
                Vec::new()
            } else {
                doc.text.lines().collect()
            };

            nodes_indexed += self.ingest_document(
                doc,
                &rel,
                &abs_root,
                &defined_raw,
                &info,
                &rel_edges,
                &text_lines,
            );
        }

        self.rebuild_reverse_deps();

        Ok(ScanSummary {
            files_scanned,
            nodes_indexed,
            total_symbols: self.nodes.read().unwrap().len(),
        })
    }
}

impl AstIndex {
    /// Index one document, returning how many nodes it produced. Private to the
    /// ingestion; the public entry points drive it per document.
    #[allow(clippy::too_many_arguments)]
    fn ingest_document(
        &self,
        doc: &scip::types::Document,
        rel: &str,
        abs_root: &Path,
        defined_raw: &HashSet<String>,
        info: &HashMap<String, (String, String)>,
        rel_edges: &HashMap<String, HashSet<String>>,
        text_lines: &[&str],
    ) -> usize {
        // Definitions in this document, with their body spans.
        let mut defs: Vec<Def> = Vec::new();
        for occ in &doc.occurrences {
            if occ.symbol_roles & ROLE_DEFINITION == 0 || render_symbol(&occ.symbol).is_none() {
                continue;
            }
            let Some(decl) = line_span(&occ.range) else {
                continue;
            };
            let enclosing = line_span(&occ.enclosing_range);
            let body = enclosing.unwrap_or(decl);
            let (mut kind, signature) = info
                .get(&occ.symbol)
                .cloned()
                .unwrap_or_else(|| ("function".to_string(), String::new()));
            // A test is marked by the SCIP Test role where the indexer sets it,
            // scip-java does for JUnit. rust-analyzer does not set it for a
            // `#[test]`, so fall back to axiom's own heuristic, the same one its
            // scan uses: a test file, or a name that starts with `test_`, which
            // is how the Rust parser keys on `#[test]`. This decides only the
            // kind label; the edges are resolved either way.
            if occ.symbol_roles & ROLE_TEST != 0 {
                kind = "test".to_string();
            } else if kind != "test" {
                let key = render_symbol(&occ.symbol).unwrap_or_default();
                let leaf = key.rsplit(['.', '#', ':']).next().unwrap_or("");
                if Self::is_test_path_or_file(rel) || leaf.starts_with("test_") {
                    kind = "test".to_string();
                }
            }
            defs.push(Def {
                raw: occ.symbol.clone(),
                decl,
                body,
                widen: enclosing.is_none(),
                kind,
                signature,
            });
        }

        // Widen a body that came only from the declaration line to reach the
        // next definition, so a reference below the declaration is still charged
        // to it. A definition that carried an enclosing_range keeps it.
        let mut starts: Vec<usize> = defs.iter().map(|d| d.decl.0).collect();
        starts.sort_unstable();
        for d in defs.iter_mut() {
            if d.widen {
                let next = starts.iter().find(|&&s| s > d.decl.0).copied();
                let end = next.map(|n| n.saturating_sub(1)).unwrap_or(usize::MAX);
                d.body = (d.decl.0, end.max(d.decl.1));
            }
        }

        // Charge each reference to the innermost definition that contains it,
        // and keep the edge when its target is defined in the index.
        let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
        for occ in &doc.occurrences {
            if occ.symbol_roles & ROLE_DEFINITION != 0 || !defined_raw.contains(&occ.symbol) {
                continue;
            }
            let Some(rng) = line_span(&occ.range) else {
                continue;
            };
            let owner = defs
                .iter()
                .filter(|d| contains(d.body, rng))
                .min_by_key(|d| d.body.1.saturating_sub(d.body.0))
                .map(|d| d.raw.clone());
            if let Some(owner_raw) = owner {
                if owner_raw != occ.symbol {
                    edges
                        .entry(owner_raw)
                        .or_default()
                        .insert(occ.symbol.clone());
                }
            }
        }

        // Insert the nodes. Attribution to the file is by the thread-local the
        // parsers use, so file_to_symbols and symbol_to_file are set.
        crate::PARSING_FILE.with(|f| *f.borrow_mut() = Some(rel.to_string()));
        let mut count = 0usize;
        for d in &defs {
            let Some(key) = render_symbol(&d.raw) else {
                continue;
            };
            let mut deps: Vec<String> = edges
                .get(&d.raw)
                .into_iter()
                .flatten()
                .chain(rel_edges.get(&d.raw).into_iter().flatten())
                .filter_map(|t| render_symbol(t))
                .filter(|t| t != &key)
                .collect();
            deps.sort();
            deps.dedup();

            let body = if text_lines.is_empty() {
                String::new()
            } else {
                let end = d.body.1.min(text_lines.len().saturating_sub(1));
                if d.body.0 <= end {
                    text_lines[d.body.0..=end].join("\n")
                } else {
                    String::new()
                }
            };
            let content = if d.signature.is_empty() {
                key.clone()
            } else {
                d.signature.clone()
            };
            self.index_node_at(&key, &d.kind, &content, &body, deps, Some(d.decl));
            count += 1;
        }
        crate::PARSING_FILE.with(|f| *f.borrow_mut() = None);

        self.file_roots
            .write()
            .unwrap()
            .insert(rel.to_string(), abs_root.to_path_buf());

        count
    }
}
