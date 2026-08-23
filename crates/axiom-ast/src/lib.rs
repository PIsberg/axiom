use axiom_proto::AstNode;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Hold an exclusive lock beside the index while it is read and rewritten.
///
/// Two agents sharing one workspace both load the index, both write it whole,
/// and the second write erases the first agent's node. That is not a merge
/// conflict anyone can see: the work simply disappears. Serialising the
/// read-modify-write is what stops it.
///
/// The lock is a file created exclusively, so acquiring it is atomic. A lock
/// older than the timeout is taken over rather than waited on forever, since a
/// crashed process cannot release its own.
pub struct IndexLock {
    path: PathBuf,
    token: String,
}

impl IndexLock {
    /// How long a lock may sit untouched before another agent takes it over.
    ///
    /// Every operation this guards is a read, an edit and a write of one small
    /// file, which takes single-digit milliseconds. Two seconds is already two
    /// orders of magnitude beyond that, and it is what an agent waits after a
    /// holder dies. Thirty seconds, the previous value, stalled every other
    /// agent for half a minute per operation after a single crash.
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

    /// How long to keep waiting for a live holder before giving up.
    const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

    pub fn acquire(index_path: &Path) -> std::io::Result<Self> {
        let path = index_path.with_extension("lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| explain_denied(parent, e))?;
        }

        // Written into the lock and read back, so an agent can tell its own lock
        // from one another agent created in the same instant. Two waiters can
        // both decide a lock is stale; only one wins the create, but without a
        // token the loser cannot tell that it lost.
        let token = format!("{}-{:?}", std::process::id(), std::thread::current().id());

        let start = std::time::Instant::now();
        let mut announced = false;

        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(token.as_bytes())?;
                    drop(file);

                    // Confirm the lock still says what we wrote. If another agent
                    // judged ours stale and replaced it, we do not hold it.
                    match std::fs::read_to_string(&path) {
                        Ok(found) if found == token => return Ok(Self { path, token }),
                        _ => continue,
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let untouched_for = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default())
                        .unwrap_or_default();

                    if untouched_for > Self::STALE_AFTER {
                        // The holder is gone or wedged. Take it over rather than
                        // wait out a process that is never coming back.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }

                    if start.elapsed() > Self::GIVE_UP_AFTER {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "gave up after {:?} waiting for {:?}, which another agent is holding",
                                Self::GIVE_UP_AFTER, path
                            ),
                        ));
                    }

                    // A wait long enough to notice should not be silent.
                    if !announced && start.elapsed() > std::time::Duration::from_millis(500) {
                        announced = true;
                        eprintln!("waiting for {path:?}, held by another agent");
                    }

                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                // Creating the lock can hit the same transient sharing
                // violation the rename does, when another agent is replacing
                // the file this lock guards. Transient means retry, not fail.
                Err(e) if worth_retrying(&e) && start.elapsed() < Self::GIVE_UP_AFTER => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        // Only remove the lock if it is still ours. If another agent judged it
        // stale and took over, deleting it here would release that agent's lock
        // rather than our own.
        match std::fs::read_to_string(&self.path) {
            Ok(found) if found == self.token => {
                let _ = std::fs::remove_file(&self.path);
            }
            _ => {}
        }
    }
}

/// Whether a filesystem error is worth trying again.
///
/// On Windows, replacing or creating a file another process holds open fails
/// with a sharing violation that surfaces as PermissionDenied, and it clears as
/// soon as that handle closes. On Unix there is no such rule: a rename succeeds
/// with readers attached, and EACCES means the directory is not writable, which
/// waiting will not change. Retrying it there converts an immediate, accurate
/// error into a long pause followed by the same error.
///
/// Everything else is treated as final on both. A cross-device rename or a full
/// disk will not start working within five seconds, and pretending otherwise
/// only delays the report.
fn worth_retrying(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::Interrupted {
        return true;
    }
    cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Write a file so a reader sees either the old contents or the new ones, never
/// a half-written file: write beside the target, then rename over it.
///
/// This matters most for files that are rewritten whole. The ledger and the
/// operation log are JSON arrays, so a process killed part-way through writing
/// one leaves a document that does not parse, and every record in it is lost
/// rather than just the one being appended.
/// Turn a bare "Access is denied" into the diagnosis it almost always has on
/// Windows.
///
/// A process launched from a file carrying a Low mandatory integrity label runs
/// at low integrity: it reads anything and creates nothing, so every write fails
/// with this error wherever it points, including directories the same user can
/// plainly write to from a shell. A binary inherits that label from the
/// directory it was built into, and a cargo output directory first created by a
/// sandboxed process carries it, as does every artifact cargo later writes
/// there. The bare error sends the reader hunting for a filesystem permission
/// problem that is not there; naming the cause costs one string.
pub fn explain_denied(path: &Path, error: std::io::Error) -> std::io::Error {
    if error.kind() != std::io::ErrorKind::PermissionDenied {
        return error;
    }
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "cannot write {}: {error}. If this process cannot write anywhere, not \
             just here, the binary is probably running at low integrity: run \
             `icacls` on the directory holding the executable and look for \
             `Mandatory Label\\Low Mandatory Level`. Rebuilding into a directory \
             without that label clears it; raising the label in place needs \
             SeRelabelPrivilege.",
            path.display()
        ),
    )
}
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| explain_denied(&tmp, e))?;

    // Renaming over a file another process currently has open fails on Windows
    // with a sharing violation, and readers of these files are common: an agent
    // polling the ledger is enough. The violation lasts only as long as that
    // handle, so retry briefly rather than failing the write.
    //
    // Measured before this loop existed: twenty agents attesting while three
    // threads read the ledger lost sixteen of the twenty records to "Access is
    // denied", which is a worse outcome than the torn write the rename prevents.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut wait = std::time::Duration::from_millis(1);
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if worth_retrying(&e) && std::time::Instant::now() < deadline => {
                std::thread::sleep(wait);
                // Back off, but stay well below the deadline so a contended file
                // still gets many attempts.
                wait = (wait * 2).min(std::time::Duration::from_millis(20));
                let _ = e;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// On-disk index format version. Bumped when the shape below changes in a way
/// an older reader could misinterpret.
const INDEX_FORMAT_VERSION: u32 = 2;

/// What `.axiom/index.json` holds. The nodes alone are not enough: blast radius
/// resolves accessor calls through `method_return_types` and `file_call_names`,
/// so an index that persists only nodes silently loses that resolution the
/// moment it crosses a process boundary.
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedIndex {
    #[serde(default)]
    format_version: u32,
    nodes: HashMap<String, AstNode>,
    #[serde(default)]
    method_return_types: HashMap<String, String>,
    #[serde(default)]
    file_call_names: HashMap<String, Vec<String>>,
    #[serde(default)]
    file_to_symbols: HashMap<String, Vec<String>>,
}

/// Merkle AST Content-Addressable Store (CAS), Symbol Graph & Zoekt Trigram Index
pub struct AstIndex {
    nodes: RwLock<HashMap<String, AstNode>>,
    /// Reverse call graph: caller_symbol -> list of test/dependent symbols
    reverse_deps: RwLock<HashMap<String, Vec<String>>>,
    /// Global pre-compiled CAS: AST node hash -> compiled artifact digest
    cas_cache: RwLock<HashMap<String, String>>,
    /// Zoekt-style in-memory file text & trigram index
    zoekt_index: RwLock<ZoektIndex>,
    /// Accessor method return-type map: method_name -> declared return type
    method_return_types: RwLock<HashMap<String, String>>,
    /// Method names invoked in each file, taken from comment- and string-stripped
    /// source. Persisted in place of the raw text: only membership is ever tested,
    /// and the vocabulary is a small fraction of the source it is derived from.
    file_call_names: RwLock<HashMap<String, Vec<String>>>,
    /// File path to indexed symbols mapping
    file_to_symbols: RwLock<HashMap<String, Vec<String>>>,
    /// Symbols and files this process has deliberately forgotten since it loaded.
    ///
    /// Saving has to merge rather than overwrite, or a scan running beside
    /// another agent writes back its own view and drops that agent's work. But a
    /// plain union would also resurrect everything a re-scan just purged, so the
    /// removals are recorded and subtracted from the merge.
    forgotten_symbols: RwLock<HashSet<String>>,
    forgotten_files: RwLock<HashSet<String>>,
    /// The file currently being parsed, so every symbol it produces is attributed
    /// to it whichever language parser produced it.
    ///
    /// Recording this inside each parser meant only the Java one did, so a
    /// deleted .rs or .py file left its symbols behind for ever: the purge works
    /// by looking up what a file owned, and for those languages the answer was
    /// always nothing.
    parsing_file: RwLock<Option<String>>,
    /// The line each symbol was declared on, used to attribute a call site to
    /// the function it sits inside rather than to the whole file.
    ///
    /// Scan-scoped and not persisted: it exists only long enough for
    /// `resolve_reference_edges` to turn references into dependencies, which
    /// are what survive to disk.
    symbol_lines: RwLock<HashMap<String, usize>>,
    /// Every name each file mentions, with the line it was mentioned on.
    ///
    /// Collected while parsing and resolved once the whole tree has been read,
    /// because a file that references a symbol defined in a file scanned later
    /// cannot be resolved when it is read.
    pending_refs: RwLock<HashMap<String, Vec<(usize, String)>>>,
}

impl Default for AstIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl AstIndex {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            reverse_deps: RwLock::new(HashMap::new()),
            cas_cache: RwLock::new(HashMap::new()),
            zoekt_index: RwLock::new(ZoektIndex::new()),
            method_return_types: RwLock::new(HashMap::new()),
            file_call_names: RwLock::new(HashMap::new()),
            file_to_symbols: RwLock::new(HashMap::new()),
            forgotten_symbols: RwLock::new(HashSet::new()),
            forgotten_files: RwLock::new(HashSet::new()),
            parsing_file: RwLock::new(None),
            symbol_lines: RwLock::new(HashMap::new()),
            pending_refs: RwLock::new(HashMap::new()),
        }
    }

    /// Insert or update an AST Node into the Merkle index.
    pub fn index_node(
        &self,
        symbol: &str,
        kind: &str,
        content: &str,
        deps: Vec<String>,
    ) -> AstNode {
        self.index_node_at(symbol, kind, content, deps, None)
    }

    /// Insert a node, recording which line it was declared on.
    ///
    /// The line is what lets a reference found later be charged to the function
    /// it sits in. Without it the only available owner is the file, and in a
    /// language where one file holds forty unrelated tests, charging all of
    /// them for one reference is the same as charging none of them.
    pub fn index_node_at(
        &self,
        symbol: &str,
        kind: &str,
        content: &str,
        deps: Vec<String>,
        declared_on: Option<usize>,
    ) -> AstNode {
        if let Some(line) = declared_on {
            self.symbol_lines
                .write()
                .unwrap()
                .insert(symbol.to_string(), line);
        }
        let normalized = content.trim();
        let mut hasher = blake3::Hasher::new();
        hasher.update(normalized.as_bytes());
        for dep in &deps {
            hasher.update(dep.as_bytes());
        }
        let hash = hasher.finalize().to_hex().to_string();

        let node = AstNode {
            id: format!("node_{}", &hash[..12]),
            symbol_path: symbol.to_string(),
            kind: kind.to_string(),
            hash: hash.clone(),
            source_range: (0, content.len()),
            docstring: None,
            signature: Some(symbol.to_string()),
            dependencies: deps.clone(),
        };

        // Update reverse dependencies for blast-radius calculation
        let mut rev = self.reverse_deps.write().unwrap();
        for dep in &deps {
            rev.entry(dep.clone()).or_default().push(symbol.to_string());
        }

        let mut nodes = self.nodes.write().unwrap();
        nodes.insert(symbol.to_string(), node.clone());
        drop(nodes);

        // Attribution happens here rather than in each parser, so a language
        // added later cannot forget to do it.
        if let Some(file) = self.parsing_file.read().unwrap().as_ref() {
            let mut owned = self.file_to_symbols.write().unwrap();
            let entry = owned.entry(file.clone()).or_default();
            if !entry.iter().any(|s| s == symbol) {
                entry.push(symbol.to_string());
            }
        }

        node
    }

    /// Compute real BLAKE3 Merkle root over all indexed AST node hashes in the CAS
    pub fn compute_merkle_root(&self) -> String {
        let nodes = self.nodes.read().unwrap();
        if nodes.is_empty() {
            return "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        }

        let mut sorted_keys: Vec<&String> = nodes.keys().collect();
        sorted_keys.sort();

        let mut hasher = blake3::Hasher::new();
        for key in sorted_keys {
            if let Some(node) = nodes.get(key) {
                hasher.update(node.symbol_path.as_bytes());
                hasher.update(node.hash.as_bytes());
            }
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Lookup a symbol in the AST index (supports exact match and class-level matching)
    /// Look a symbol up, exactly if possible and by unique suffix otherwise.
    ///
    /// Returns nothing when the name is blank or matches more than one symbol.
    /// The previous version returned the first match found while walking a
    /// HashMap, which meant two different wrong answers: an empty name matched
    /// everything, because every string ends with the empty string, so a request
    /// that forgot its argument got a real-looking node for an arbitrary symbol;
    /// and an ambiguous suffix like "execute" silently resolved to whichever
    /// class the iteration order happened to reach first, differently between
    /// runs.
    pub fn get_symbol(&self, symbol_path: &str) -> Option<AstNode> {
        if symbol_path.trim().is_empty() {
            return None;
        }

        let nodes = self.nodes.read().unwrap();
        if let Some(node) = nodes.get(symbol_path) {
            return Some(node.clone());
        }

        let mut matches: Vec<&String> = nodes
            .keys()
            .filter(|k| Self::is_suffix_match(k, symbol_path))
            .collect();

        if matches.len() == 1 {
            return nodes.get(matches.pop().unwrap()).cloned();
        }

        None
    }

    /// Every symbol path in the index, sorted.
    ///
    /// Exposed for tests that assert a property of the whole index rather than
    /// of one lookup, such as "no symbol name carries a file path", which is a
    /// regression this parser has produced before.
    pub fn symbol_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self.nodes.read().unwrap().keys().cloned().collect();
        out.sort();
        out
    }

    /// Every symbol an ambiguous name could have meant, sorted so a caller can
    /// show a stable list rather than an arbitrary one.
    pub fn candidates_for(&self, symbol_path: &str) -> Vec<String> {
        if symbol_path.trim().is_empty() {
            return Vec::new();
        }
        let nodes = self.nodes.read().unwrap();
        let mut out: Vec<String> = nodes
            .keys()
            .filter(|k| Self::is_suffix_match(k, symbol_path))
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// Whether `key` names the same thing as `symbol_path` written in short.
    ///
    /// A shorter name has to end on a boundary in the key: `alpha` matches
    /// `pkg.Class::alpha` and `src/lib.rs::alpha` matches the absolute path it
    /// was recorded under, while `pha` matches neither. Requiring the boundary
    /// is what keeps this from degenerating into "ends with", which is true of
    /// every key when the name is empty.
    fn is_suffix_match(key: &str, symbol_path: &str) -> bool {
        if key == symbol_path || key.starts_with(&format!("{symbol_path}::")) {
            return true;
        }
        match key.strip_suffix(symbol_path) {
            Some(before) => before.ends_with('.') || before.ends_with('/') || before.ends_with(':'),
            None => false,
        }
    }

    /// List all symbols currently indexed
    pub fn list_symbols(&self) -> Vec<AstNode> {
        let nodes = self.nodes.read().unwrap();
        nodes.values().cloned().collect()
    }

    /// Total number of indexed symbols
    pub fn total_symbols_count(&self) -> usize {
        let nodes = self.nodes.read().unwrap();
        nodes.len()
    }

    /// Total number of test symbols in repository (class kind 'test' or method kind 'test')
    pub fn total_tests_count(&self) -> usize {
        let nodes = self.nodes.read().unwrap();
        nodes.values().filter(|n| n.kind == "test").count()
    }

    /// Trigram / Text search across indexed codebase
    pub fn search_symbols_and_text(&self, query: &str, max_results: usize) -> Vec<ZoektMatch> {
        let zoekt = self.zoekt_index.read().unwrap();
        zoekt.search(query, None, max_results)
    }

    /// Search source text, falling back to symbol names when the text yields
    /// nothing. An invalid pattern is an error, never a silent literal search:
    /// answering a different question than the one asked is worse than refusing.
    pub fn search(
        &self,
        query: &str,
        mode: SearchMode,
        max_results: usize,
    ) -> Result<(SearchMode, Vec<ZoektMatch>), String> {
        let effective = match mode {
            SearchMode::Auto => {
                if looks_like_a_pattern(query) && Regex::new(query).is_ok() {
                    SearchMode::Regex
                } else {
                    SearchMode::Literal
                }
            }
            explicit => explicit,
        };

        let compiled = match effective {
            SearchMode::Regex => Some(
                Regex::new(query)
                    .map_err(|e| format!("{:?} is not a valid regular expression: {}", query, e))?,
            ),
            _ => None,
        };

        Ok((
            effective,
            self.run_search(query, compiled.as_ref(), max_results),
        ))
    }

    fn run_search(
        &self,
        query: &str,
        compiled: Option<&Regex>,
        max_results: usize,
    ) -> Vec<ZoektMatch> {
        let zoekt = self.zoekt_index.read().unwrap();
        let matches = zoekt.search(query, compiled, max_results);
        if !matches.is_empty() {
            return matches;
        }

        let nodes = self.nodes.read().unwrap();
        let mut results = Vec::new();
        for (sym, node) in nodes.iter() {
            let signature = node.signature.as_deref().unwrap_or("");
            let hit = match compiled {
                Some(re) => re.is_match(sym) || re.is_match(signature),
                None => sym.contains(query) || signature.contains(query),
            };
            if hit {
                // No file or line to point at: this matched a symbol name, not a
                // line of source. Reporting line 1 of the symbol path as though it
                // were a file sends a caller looking somewhere that does not exist.
                results.push(ZoektMatch {
                    match_kind: "symbol".to_string(),
                    file_path: sym.clone(),
                    line_number: None,
                    line_content: node.signature.clone().unwrap_or_else(|| sym.clone()),
                });
                if results.len() >= max_results {
                    break;
                }
            }
        }
        results
    }

    /// The file extension of the source a symbol came from, when the index knows
    /// it. Used to keep language-specific tooling from being pointed at a
    /// language it cannot handle.
    /// The extension of the file a symbol was indexed from.
    ///
    /// The name is resolved first, so a caller that asked about `is_open`
    /// rather than the full key gets the same answer. Comparing the caller's
    /// spelling directly against the stored keys returned `None` for every
    /// short name, and a `None` here reads as "no language known", which sends
    /// a Python symbol to the Rust compiler.
    /// The file a symbol was indexed from.
    ///
    /// Needed by anything that has to reach the source behind a symbol rather
    /// than the symbol's own record: mutating it, for instance, to find out what
    /// really breaks when it changes.
    pub fn file_of_symbol(&self, symbol_path: &str) -> Option<String> {
        let canonical = self.get_symbol(symbol_path)?.symbol_path;
        let file_syms = self.file_to_symbols.read().unwrap();
        for (file, symbols) in file_syms.iter() {
            if symbols.iter().any(|s| s == &canonical) {
                return Some(file.clone());
            }
        }
        None
    }

    /// Every test symbol in the index, sorted.
    pub fn test_symbol_paths(&self) -> Vec<String> {
        let nodes = self.nodes.read().unwrap();
        let mut out: Vec<String> = nodes
            .values()
            .filter(|n| n.kind == "test")
            .map(|n| n.symbol_path.clone())
            .collect();
        out.sort();
        out
    }

    pub fn language_of_symbol(&self, symbol_path: &str) -> Option<String> {
        let canonical = self.get_symbol(symbol_path)?.symbol_path;
        let file_syms = self.file_to_symbols.read().unwrap();
        for (file, symbols) in file_syms.iter() {
            if symbols.iter().any(|s| s == &canonical) {
                return Path::new(file)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string());
            }
        }
        None
    }

    /// A cheap summary of every source file under `root`: path, size and
    /// modification time. Comparing two of these says whether a re-scan is
    /// worth doing, at the cost of a stat per file rather than a parse.
    pub fn tree_fingerprint(&self, root: &Path) -> String {
        let mut entries: Vec<String> = Vec::new();
        Self::fingerprint_dir(root, &mut entries);
        entries.sort();

        let mut hasher = blake3::Hasher::new();
        for e in &entries {
            hasher.update(e.as_bytes());
            hasher.update(
                b"
",
            );
        }
        hasher.finalize().to_hex().to_string()
    }

    fn fingerprint_dir(dir: &Path, out: &mut Vec<String>) {
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "build"
                    || name == "dist"
                {
                    continue;
                }
                Self::fingerprint_dir(&path, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if !matches!(
                    ext,
                    "java"
                        | "rs"
                        | "py"
                        | "js"
                        | "ts"
                        | "go"
                        | "kt"
                        | "scala"
                        | "c"
                        | "cpp"
                        | "h"
                        | "json"
                        | "toml"
                ) {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                out.push(format!(
                    "{}|{}|{}",
                    path.to_string_lossy(),
                    meta.len(),
                    modified
                ));
            }
        }
    }

    /// Predictive Blast-Radius Calculation with Accessor Return-Type Resolution
    /// How far the traversal looks beyond what it reports, so a caller can see
    /// which tests widening the depth would add.
    const SURVEY_DEPTH: usize = 3;

    pub fn compute_blast_radius(
        &self,
        symbol_path: &str,
        max_depth: usize,
    ) -> Option<BlastRadiusResult> {
        let symbol_node = self.get_symbol(symbol_path)?;
        let canonical_symbol = symbol_node.symbol_path;
        let simple_name = Self::simple_name_of(&canonical_symbol);

        let rev = self.reverse_deps.read().unwrap();
        let nodes = self.nodes.read().unwrap();
        let mrt = self.method_return_types.read().unwrap();
        let file_calls = self.file_call_names.read().unwrap();

        let mut visited = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut impacted_tests: Vec<String> = Vec::new();

        // Seed queue with canonical symbol, unqualified name, and class prefix
        queue.push_back((canonical_symbol.clone(), 0));
        visited.insert(canonical_symbol.clone());

        if simple_name != canonical_symbol {
            queue.push_back((simple_name.to_string(), 0));
            visited.insert(simple_name.to_string());
        }

        let class_symbol = canonical_symbol
            .split("::")
            .next()
            .unwrap_or(&canonical_symbol);
        if class_symbol != canonical_symbol && visited.insert(class_symbol.to_string()) {
            queue.push_back((class_symbol.to_string(), 0));
        }

        let mut tests_by_depth: HashMap<usize, Vec<String>> = HashMap::new();

        // Walk further than is reported. `impacted_tests` stays at `max_depth`,
        // because widening it costs precision: measured on a 2,219-test suite,
        // going from depth 1 to depth 2 took one symbol from 57 impacted tests
        // to 146 with no recall gain, and lifted the overlap between the blast
        // radii of unrelated symbols from 0.00 to 0.19.
        //
        // But a test that reaches the symbol through another class is real, and
        // reporting nothing about it leaves a caller unable to widen even when
        // it wants to. So the deeper layers are computed and returned separately
        // in `tests_by_depth`, and the caller decides. Traversal is cheap; it is
        // running the tests that is not.
        let survey_depth = max_depth.max(Self::SURVEY_DEPTH);

        while let Some((curr, depth)) = queue.pop_front() {
            if let Some(node) = nodes.get(&curr) {
                if node.kind == "test" {
                    let d = depth.max(1);
                    let already = tests_by_depth.values().any(|v| v.contains(&curr));
                    if !already {
                        tests_by_depth.entry(d).or_default().push(curr.clone());
                    }
                    if d <= max_depth && !impacted_tests.contains(&curr) {
                        impacted_tests.push(curr.clone());
                    }
                }
            }

            if depth < survey_depth {
                // The graph is keyed by the name a caller writes, and its
                // values are full symbol paths, so the next hop has to be
                // looked up under both. Looking up only the path found nothing
                // after the first step: `reverse_deps` has an entry for
                // `write_atomically`, never for
                // `crates/axiom-ast/src/lib.rs::write_atomically`. Every
                // transitive layer was silently empty for any language whose
                // symbols are keyed by file.
                let simple = Self::simple_name_of(&curr);
                let keys: [&str; 2] = [curr.as_str(), simple];
                for key in keys.iter().take(if simple == curr { 1 } else { 2 }) {
                    if let Some(callers) = rev.get(*key) {
                        for caller in callers {
                            if visited.insert(caller.clone()) {
                                queue.push_back((caller.clone(), depth + 1));
                            }
                        }
                    }
                }
            }
        }

        // Accessor Return-Type Inference: Find test files calling accessors returning simple_name
        // e.g. sharedRaceConditionDetector() -> RaceConditionDetector
        let mut accessor_names = Vec::new();
        for (m_name, ret_type) in mrt.iter() {
            if ret_type == simple_name || ret_type == &canonical_symbol {
                let short_m = m_name.split('.').next_back().unwrap_or(m_name);
                accessor_names.push(short_m.to_string());
            }
        }

        if !accessor_names.is_empty() {
            let file_syms = self.file_to_symbols.read().unwrap();
            for (file_path, calls) in file_calls.iter() {
                for acc in &accessor_names {
                    if calls.iter().any(|c| c == acc) {
                        if let Some(syms) = file_syms.get(file_path) {
                            for sym in syms {
                                if let Some(node) = nodes.get(sym) {
                                    if node.kind == "test" && !impacted_tests.contains(sym) {
                                        impacted_tests.push(sym.clone());
                                        tests_by_depth.entry(1).or_default().push(sym.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Expand any impacted test classes to include their individual test methods
        let mut method_expansions = Vec::new();
        for test_sym in &impacted_tests {
            let prefix = format!("{}::", test_sym);
            for (sym, node) in nodes.iter() {
                if node.kind == "test"
                    && sym.starts_with(&prefix)
                    && !impacted_tests.contains(sym)
                    && !method_expansions.contains(sym)
                {
                    method_expansions.push(sym.clone());
                    tests_by_depth.entry(1).or_default().push(sym.clone());
                }
            }
        }
        impacted_tests.extend(method_expansions);

        // Fallback: Whole-word reference search across all registered test nodes
        if impacted_tests.is_empty() && !simple_name.is_empty() {
            let test_pattern_1 = format!("{}Test", simple_name);
            let test_pattern_2 = format!("test{}", simple_name);
            let call_pattern_1 = format!("{}.", simple_name);
            let call_pattern_2 = format!("{}::", simple_name);
            let call_pattern_3 = format!("new {}", simple_name);
            let type_pattern = format!("{} ", simple_name);

            for (sym, node) in nodes.iter() {
                if node.kind == "test" {
                    let sig = node.signature.as_deref().unwrap_or("");
                    if (sym.contains(&test_pattern_1)
                        || sym.contains(&test_pattern_2)
                        || sym.contains(&canonical_symbol)
                        || sig.contains(&canonical_symbol)
                        || sig.contains(&call_pattern_1)
                        || sig.contains(&call_pattern_2)
                        || sig.contains(&call_pattern_3)
                        || sig.contains(&type_pattern)
                        || node
                            .dependencies
                            .iter()
                            .any(|d| d == simple_name || d == &canonical_symbol))
                        && !impacted_tests.contains(sym)
                    {
                        impacted_tests.push(sym.clone());
                        tests_by_depth.entry(1).or_default().push(sym.clone());
                    }
                }
            }
        }

        let direct_tests = tests_by_depth
            .get(&1)
            .cloned()
            .unwrap_or_else(|| impacted_tests.clone());
        let total_tests = self.total_tests_count();
        let pruned_percentage = if total_tests > 0 && !impacted_tests.is_empty() {
            let executed = impacted_tests.len().min(total_tests);
            ((total_tests - executed) as f64 / total_tests as f64) * 100.0
        } else {
            0.0
        };

        Some(BlastRadiusResult {
            symbol: canonical_symbol,
            impacted_tests,
            direct_tests,
            tests_by_depth,
            total_tests_in_repo: total_tests,
            pruned_test_percentage: pruned_percentage,
        })
    }

    /// Query global CAS cache (0ms instant re-use)
    pub fn get_cas_artifact(&self, ast_hash: &str) -> Option<String> {
        let cas = self.cas_cache.read().unwrap();
        cas.get(ast_hash).cloned()
    }

    /// Store compiled artifact in CAS
    pub fn put_cas_artifact(&self, ast_hash: &str, artifact_digest: &str) {
        let mut cas = self.cas_cache.write().unwrap();
        cas.insert(ast_hash.to_string(), artifact_digest.to_string());
    }

    /// Recursively scan and parse a real repository directory into the Merkle AST CAS
    pub fn scan_directory(&self, root: &Path) -> std::io::Result<ScanSummary> {
        let mut files_scanned = 0;
        let mut nodes_extracted = 0;
        let mut visited: HashSet<String> = HashSet::new();

        // Resolve the root once. Canonicalising every file instead costs a
        // filesystem round trip per entry, measured at 24ms/file against
        // 3.2ms/file over a 459-file tree.
        let root_key = Self::canonical_key(root);
        self.walk_dir(
            root,
            root,
            &root_key,
            &mut files_scanned,
            &mut nodes_extracted,
            &mut visited,
        )?;

        // A scan is a statement about what the tree contains now, so anything
        // recorded from a file that has since disappeared has to go. Without
        // this the index only ever grows: a deleted class stays answerable and
        // a renamed method keeps its old name alongside the new one, and the
        // blast radius then names tests that no longer exist.
        self.forget_missing_files(&root_key, &visited);
        self.rebuild_reverse_deps();

        // Only now, with every file read, can a reference be matched against
        // the symbol it names. Doing it per file resolved nothing that was
        // defined further down the walk.
        self.resolve_reference_edges();

        Ok(ScanSummary {
            files_scanned,
            nodes_indexed: nodes_extracted,
            total_symbols: self.nodes.read().unwrap().len(),
        })
    }

    /// One canonical spelling for a file, so the same file scanned as "." and as
    /// an absolute root produces the same key. Without this the index holds two
    /// records for one file, and a purge keyed on the root prefix matches
    /// neither.
    /// A file's key, built by appending its path below the walk root to the
    /// already-resolved root. Equivalent to canonicalising the file, without
    /// asking the filesystem again for every entry.
    fn key_under_root(root: &Path, root_key: &str, path: &Path) -> String {
        match path.strip_prefix(root) {
            Ok(rest) => {
                let rest = rest.to_string_lossy().replace('\\', "/");
                if rest.is_empty() {
                    root_key.to_string()
                } else {
                    format!("{}/{}", root_key, rest)
                }
            }
            // Not below the root, which the walk should make impossible; fall
            // back to resolving the file itself rather than inventing a key.
            Err(_) => Self::canonical_key(path),
        }
    }

    fn canonical_key(path: &Path) -> String {
        let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        resolved
            .to_string_lossy()
            .replace("\\", "/")
            .trim_start_matches("//?/")
            .to_string()
    }

    /// Remove everything a single file contributed to the index.
    fn forget_file(&self, file_path: &str) {
        let previous = self.file_to_symbols.write().unwrap().remove(file_path);
        self.file_call_names.write().unwrap().remove(file_path);
        self.pending_refs.write().unwrap().remove(file_path);
        self.forgotten_files
            .write()
            .unwrap()
            .insert(file_path.to_string());

        if let Some(symbols) = previous {
            let mut nodes = self.nodes.write().unwrap();
            let mut forgotten = self.forgotten_symbols.write().unwrap();
            let mut lines = self.symbol_lines.write().unwrap();
            for symbol in symbols {
                nodes.remove(&symbol);
                lines.remove(&symbol);
                forgotten.insert(symbol);
            }
        }
    }

    /// Forget files recorded under this root by an earlier scan that this one did
    /// not see and that are no longer on disk.
    ///
    /// Scoped to the root on purpose. A scan is a statement about the tree it was
    /// pointed at and says nothing about anything else, so records from other
    /// roots are left alone whether or not their files still exist. Widening this
    /// to every recorded path makes one scan able to empty an unrelated project's
    /// entries out of a shared index.
    fn forget_missing_files(&self, root_prefix: &str, visited: &HashSet<String>) {
        let recorded: Vec<String> = self
            .file_to_symbols
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        for file_path in recorded {
            if visited.contains(&file_path) {
                continue;
            }
            if !file_path.starts_with(root_prefix) {
                continue;
            }
            if !Path::new(&file_path).exists() {
                self.forget_file(&file_path);
            }
        }
    }

    /// Rebuild the reverse graph from the nodes that remain. Purging by file
    /// leaves dangling entries otherwise, and a stale caller list is how a
    /// deleted test keeps showing up in a blast radius.
    /// The last identifier in a symbol key: the name a caller writes.
    ///
    /// Two shapes reach here. A package-keyed symbol, `pkg.Class::method`,
    /// reduces to `Class`, because in Java the type is what a caller names and
    /// what a return type mentions. A file-keyed symbol,
    /// `crates/axiom-ast/src/lib.rs::write_atomically`, reduces to
    /// `write_atomically`.
    ///
    /// Telling them apart matters more than it looks. Splitting on the last dot
    /// unconditionally, which is what this used to do, took the file extension
    /// for a package separator: every Rust symbol reduced to `rs`, every Python
    /// one to `py`, and the fallback search then matched `rs::` against every
    /// symbol indexed from a `.rs` file. The blast radius for any symbol in
    /// this repository was all 49 tests, whatever was asked.
    fn simple_name_of(symbol: &str) -> &str {
        let owner = symbol.split("::").next().unwrap_or(symbol);
        // Keys always use forward slashes: `key_under_root` and `canonical_key`
        // both normalise, so a backslash never reaches here whatever host wrote
        // the index.
        let file_keyed = owner.contains('/')
            || matches!(
                owner.rsplit('.').next(),
                Some("rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "c" | "cpp" | "h")
            );

        if file_keyed {
            return symbol.rsplit("::").next().unwrap_or(symbol);
        }

        symbol
            .split('.')
            .next_back()
            .unwrap_or(symbol)
            .split("::")
            .next()
            .unwrap_or(symbol)
    }

    /// Turn the names each file mentioned into dependencies of the symbol that
    /// mentioned them.
    ///
    /// Runs once the whole tree has been read, because a file that references a
    /// symbol defined in a file scanned later cannot be resolved when it is
    /// read. A reference is kept only when some indexed symbol answers to that
    /// name: the point is a graph between things this index knows about, not a
    /// record of every word in the source.
    ///
    /// Attribution is by line. Every reference belongs to the last symbol
    /// declared above it, which is wrong for a nested function and right for
    /// everything else, and the error it makes is charging a sibling rather
    /// than charging all forty tests in the file.
    fn resolve_reference_edges(&self) {
        let pending: HashMap<String, Vec<(usize, String)>> =
            std::mem::take(&mut *self.pending_refs.write().unwrap());
        if pending.is_empty() {
            return;
        }

        let known: HashSet<String> = {
            let nodes = self.nodes.read().unwrap();
            nodes
                .keys()
                .map(|k| Self::simple_name_of(k).to_string())
                .collect()
        };

        let file_syms = self.file_to_symbols.read().unwrap().clone();
        let lines = self.symbol_lines.read().unwrap().clone();
        let mut added: HashMap<String, HashSet<String>> = HashMap::new();

        for (file, refs) in pending {
            // Symbols of this file, ordered by where they start, so the owner
            // of a line is a binary search rather than a scan.
            let mut declared: Vec<(usize, &String)> = file_syms
                .get(&file)
                .into_iter()
                .flatten()
                .filter_map(|s| lines.get(s).map(|l| (*l, s)))
                .collect();
            if declared.is_empty() {
                continue;
            }
            declared.sort();

            for (line_no, name) in refs {
                if !known.contains(&name) {
                    continue;
                }
                let owner = match declared.partition_point(|(l, _)| *l <= line_no) {
                    0 => continue, // above the first declaration: file preamble
                    i => declared[i - 1].1,
                };
                if Self::simple_name_of(owner) == name {
                    continue; // a symbol does not depend on itself
                }
                added.entry(owner.clone()).or_default().insert(name);
            }
        }

        if added.is_empty() {
            return;
        }

        let mut nodes = self.nodes.write().unwrap();
        for (symbol, names) in added {
            if let Some(node) = nodes.get_mut(&symbol) {
                let mut fresh: Vec<String> = names
                    .into_iter()
                    .filter(|n| !node.dependencies.contains(n))
                    .collect();
                if fresh.is_empty() {
                    continue;
                }
                fresh.sort();
                // The node's hash is left as parsed. It covers the declaration
                // and the imports the parser saw; a resolved edge is derived
                // from that same text, so a caller that adds a call changes the
                // caller's own hash already.
                node.dependencies.extend(fresh);
            }
        }
        drop(nodes);

        self.rebuild_reverse_deps();
        self.symbol_lines.write().unwrap().clear();
    }

    fn rebuild_reverse_deps(&self) {
        let nodes = self.nodes.read().unwrap();
        let mut rebuilt: HashMap<String, Vec<String>> = HashMap::new();
        for (symbol, node) in nodes.iter() {
            for dep in &node.dependencies {
                rebuilt.entry(dep.clone()).or_default().push(symbol.clone());
            }
        }
        *self.reverse_deps.write().unwrap() = rebuilt;
    }

    fn walk_dir(
        &self,
        dir: &Path,
        root: &Path,
        root_key: &str,
        files_count: &mut usize,
        nodes_count: &mut usize,
        visited: &mut HashSet<String>,
    ) -> std::io::Result<()> {
        if !dir.exists() || !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Skip hidden folders and build directories
                if !dir_name.starts_with('.')
                    && dir_name != "target"
                    && dir_name != "node_modules"
                    && dir_name != "build"
                    && dir_name != "dist"
                {
                    self.walk_dir(&path, root, root_key, files_count, nodes_count, visited)?;
                }
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    match ext {
                        "java" | "rs" | "py" | "js" | "ts" | "go" | "kt" | "scala" | "c"
                        | "cpp" | "h" | "json" | "toml" => {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                *files_count += 1;
                                let rel = Self::key_under_root(root, root_key, &path);
                                visited.insert(rel.clone());

                                // Drop what this file contributed last time before
                                // re-reading it, so a renamed or removed symbol does
                                // not survive alongside its replacement.
                                self.forget_file(&rel);

                                // Index into Zoekt Trigram store
                                self.zoekt_index
                                    .write()
                                    .unwrap()
                                    .add_document(&rel, &content);

                                // Index into AST CAS
                                self.parse_file_content(&rel, ext, &content, nodes_count);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    fn parse_file_content(
        &self,
        file_path: &str,
        ext: &str,
        content: &str,
        nodes_count: &mut usize,
    ) {
        *self.parsing_file.write().unwrap() = Some(file_path.to_string());
        self.parse_by_language(file_path, ext, content, nodes_count);
        *self.parsing_file.write().unwrap() = None;
        self.record_references(file_path, ext, content);
    }

    /// Record every name a file mentions, with the line it was mentioned on.
    ///
    /// Java is excluded: `parse_java_content` already collects referenced type
    /// names and attaches them to its nodes, and doing it twice would double
    /// the edges the Java tests pin.
    ///
    /// Comments and string literals are stripped first. Matching raw text made
    /// every doc comment that named a function into a call of it.
    fn record_references(&self, file_path: &str, ext: &str, content: &str) {
        if matches!(ext, "java" | "kt" | "scala") {
            return;
        }

        let clean = Self::strip_comments_and_strings(content);
        let mut refs: Vec<(usize, String)> = Vec::new();

        for (line_no, line) in clean.lines().enumerate() {
            // A call site, `name(`, is the strongest evidence that one symbol
            // uses another. Bare identifiers would also catch a type used as a
            // parameter, and would catch every local variable with them.
            for name in Self::extract_call_names(line) {
                refs.push((line_no, name));
            }

            // Types are not called, so they need the qualified forms too:
            // `Type::assoc`, `Type {`, and `Type::` all name a type that a
            // call-site scan alone would miss.
            for word in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if word.len() > 1
                    && word.chars().next().is_some_and(|c| c.is_uppercase())
                    && Self::is_valid_identifier(word)
                {
                    refs.push((line_no, word.to_string()));
                }
            }
        }

        refs.sort();
        refs.dedup();
        self.pending_refs
            .write()
            .unwrap()
            .insert(file_path.to_string(), refs);
    }

    fn parse_by_language(
        &self,
        file_path: &str,
        ext: &str,
        content: &str,
        nodes_count: &mut usize,
    ) {
        match ext {
            "java" | "kt" | "scala" => self.parse_java_content(file_path, content, nodes_count),
            "rs" => self.parse_rust_content(file_path, content, nodes_count),
            "py" => self.parse_python_content(file_path, content, nodes_count),
            "ts" | "js" => self.parse_ts_js_content(file_path, content, nodes_count),
            "go" => self.parse_go_content(file_path, content, nodes_count),
            _ => {}
        }
    }

    /// Does this line declare a Kotlin `fun` or a Scala `def`?
    ///
    /// Checked against the tokens before the parameter list rather than against
    /// the whole line, so `foo(fun_arg)` and a string mentioning `def` do not
    /// match. Modifiers are allowed before it: `private`, `override`, `suspend`,
    /// `inline`, `@main` and the rest all sit to the left of the keyword.
    ///
    /// Expression bodies are the reason this cannot simply look for a brace.
    /// `fun isOpen(depth: Int): Boolean = depth > 0` opens none, and it is the
    /// commonest shape in both languages.
    fn declares_fun_or_def(trimmed: &str) -> bool {
        if !trimmed.contains('(') {
            return false;
        }
        trimmed
            .split('(')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .any(|t| t == "fun" || t == "def")
    }

    fn is_test_path_or_file(file_path: &str) -> bool {
        let normalized = file_path.replace('\\', "/");
        let file_name = normalized.split('/').next_back().unwrap_or("");
        let fn_lower = file_name.to_lowercase();
        let is_test_filename = fn_lower.starts_with("test_")
            || fn_lower.ends_with("_test.rs")
            || fn_lower.ends_with("_test.go")
            || fn_lower.ends_with("_test.py")
            || fn_lower.ends_with(".test.ts")
            || fn_lower.ends_with(".spec.ts")
            || fn_lower.ends_with(".test.js")
            || fn_lower.ends_with(".spec.js")
            || (file_name.ends_with("Test.java")
                || file_name.ends_with("Tests.java")
                || file_name.ends_with("TestCase.java")
                || file_name.ends_with("IT.java"));

        // If file is in src/main/, it is never in a test directory
        if normalized.contains("/src/main/") && !is_test_filename {
            return false;
        }

        let in_test_dir = normalized.contains("/src/test/")
            || normalized.contains("/tests/")
            || normalized.contains("/test/")
            || normalized.contains("/__tests__/");

        in_test_dir || is_test_filename
    }

    fn is_valid_identifier(name: &str) -> bool {
        let mut chars = name.chars();
        match chars.next() {
            Some(c) if c.is_alphabetic() || c == '_' => {
                chars.all(|c| c.is_alphanumeric() || c == '_')
            }
            _ => false,
        }
    }

    fn is_java_keyword(word: &str) -> bool {
        matches!(
            word,
            "catch"
                | "return"
                | "super"
                | "this"
                | "synchronized"
                | "try"
                | "if"
                | "while"
                | "for"
                | "switch"
                | "throw"
                | "new"
                | "else"
                | "finally"
                | "assert"
                | "case"
                | "default"
                | "import"
                | "package"
                | "class"
                | "interface"
                | "enum"
                | "record"
                | "break"
                | "continue"
                | "instanceof"
                | "do"
                | "goto"
                | "const"
                | "throws"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "final"
                | "abstract"
        )
    }

    fn strip_comments_and_strings(content: &str) -> String {
        let mut result = String::with_capacity(content.len());
        let chars: Vec<char> = content.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
                // Line comment: skip until newline
                i += 2;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                if i < chars.len() {
                    result.push('\n');
                    i += 1;
                }
            } else if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
                // Block comment: skip until */
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    if chars[i] == '\n' {
                        result.push('\n');
                    } else {
                        result.push(' ');
                    }
                    i += 1;
                }
                if i + 1 < chars.len() {
                    i += 2; // skip */
                }
            } else if chars[i] == '"' {
                // String literal: skip until closing quote (accounting for escapes)
                result.push(' ');
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 2;
                    } else {
                        if chars[i] == '\n' {
                            result.push('\n');
                        } else {
                            result.push(' ');
                        }
                        i += 1;
                    }
                }
                if i < chars.len() {
                    i += 1; // skip closing quote
                }
                result.push(' ');
            } else if chars[i] == '\'' {
                // Char literal
                result.push(' ');
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < chars.len() {
                    i += 1;
                }
                result.push(' ');
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        result
    }

    fn parse_java_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let is_test_file = Self::is_test_path_or_file(file_path);

        // This parser reads Java, Kotlin and Scala. `object` and `trait` declare
        // a type in the last two and nothing in Java, so they are recognised
        // only for the extensions that have them.
        //
        // Gated on the extension rather than added to the list for everyone,
        // because loosening a match here has form: a javadoc mention once
        // hijacked an enclosing class name, and `new Foo(...)` was once indexed
        // as a method. Java's behaviour cannot change if Java never sees the
        // extra keywords.
        //
        // Without this a Scala file indexed nothing at all: `object ScalaGate`
        // matched no type keyword, so no symbol existed to ask about and the
        // Scala evaluator could not be reached through one.
        let scala_or_kotlin = matches!(
            std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str()),
            Some("scala") | Some("sc") | Some("kt") | Some("kts")
        );
        // Kotlin and Scala allow a definition with no enclosing type. Java does
        // not, so the existing path simply skips anything with an empty
        // `enclosing_class`, and every top-level `fun` and `def` went unindexed.
        //
        // The file stem stands in as the owner, which is close to what Kotlin
        // does itself: a top-level `fun` in Gate.kt compiles into `GateKt`. It
        // is validated as an identifier first, because the failure this parser
        // has actually had is writing a machine-absolute path into a symbol
        // name when the owner was empty.
        let file_stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| Self::is_valid_identifier(s))
            .unwrap_or("")
            .to_string();

        let type_keywords: &[&str] = if scala_or_kotlin {
            &["class", "interface", "enum", "record", "object", "trait"]
        } else {
            &["class", "interface", "enum", "record"]
        };
        let mut imports = Vec::new();
        let mut class_stack: Vec<(String, usize)> = Vec::new(); // (class_name, open_brace_depth)
        let mut current_brace_depth: usize = 0;

        // Extract package first
        let mut package = String::new();
        for line in content.lines() {
            let tr = line.trim();
            if tr.starts_with("package ") {
                package = tr
                    .replace("package ", "")
                    .replace(';', "")
                    .trim()
                    .to_string();
                break;
            }
        }

        // Strip comments and string literals to eliminate false edges from prose/javadoc
        let clean_code = Self::strip_comments_and_strings(content);

        // Scan stripped code for referenced type identifiers (same-package, imported, FQN, and .class literals)
        let mut referenced_types: HashSet<String> = HashSet::new();
        for word in clean_code.split(|c: char| !c.is_alphanumeric() && c != '.' && c != '_') {
            if !word.is_empty() {
                if word.contains('.') {
                    let parts: Vec<&str> = word.split('.').collect();
                    for (idx, &part) in parts.iter().enumerate() {
                        if !part.is_empty()
                            && part.chars().next().unwrap().is_uppercase()
                            && Self::is_valid_identifier(part)
                        {
                            referenced_types.insert(part.to_string());
                            let prefix = parts[..=idx].join(".");
                            referenced_types.insert(prefix);
                        }
                    }
                } else if word.chars().next().unwrap().is_uppercase()
                    && Self::is_valid_identifier(word)
                {
                    referenced_types.insert(word.to_string());
                    if !package.is_empty() {
                        referenced_types.insert(format!("{}.{}", package, word));
                    }
                }
            }
        }

        // Record which methods this file calls, for accessor resolution.
        self.file_call_names
            .write()
            .unwrap()
            .insert(file_path.to_string(), Self::extract_call_names(&clean_code));

        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("*/")
            {
                i += 1;
                continue;
            }

            if trimmed.starts_with("import ") {
                let imp = trimmed
                    .replace("import ", "")
                    .replace("static ", "")
                    .replace(';', "")
                    .trim()
                    .to_string();
                imports.push(imp);
            } else if type_keywords
                .iter()
                .any(|k| trimmed.contains(&format!("{k} ")))
                && (trimmed.starts_with("public ")
                    || trimmed.starts_with("private ")
                    || trimmed.starts_with("protected ")
                    || trimmed.starts_with("abstract ")
                    || trimmed.starts_with("final ")
                    || trimmed.starts_with("static ")
                    || trimmed.starts_with("@interface ")
                    || type_keywords
                        .iter()
                        .any(|k| trimmed.starts_with(&format!("{k} "))))
            {
                let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                if let Some(pos) = tokens.iter().position(|t| type_keywords.contains(t)) {
                    if pos + 1 < tokens.len() {
                        let raw_name = tokens[pos + 1]
                            .split('<')
                            .next()
                            .unwrap_or("")
                            .replace('{', "");
                        let class_name = raw_name.trim();
                        if Self::is_valid_identifier(class_name) {
                            let open_c = trimmed.chars().filter(|&c| c == '{').count();
                            let decl_depth = current_brace_depth + open_c.max(1);
                            class_stack.push((class_name.to_string(), decl_depth));

                            let full_symbol = if package.is_empty() {
                                class_name.to_string()
                            } else {
                                format!("{}.{}", package, class_name)
                            };

                            let kind = if is_test_file
                                && (class_name.ends_with("Test")
                                    || class_name.ends_with("Tests")
                                    || class_name.ends_with("TestCase"))
                            {
                                "test"
                            } else {
                                "class"
                            };

                            let mut node_deps = imports.clone();
                            for ref_t in &referenced_types {
                                if !node_deps.contains(ref_t)
                                    && ref_t != &full_symbol
                                    && ref_t != class_name
                                {
                                    node_deps.push(ref_t.clone());
                                }
                            }

                            self.index_node(&full_symbol, kind, trimmed, node_deps);
                            self.file_to_symbols
                                .write()
                                .unwrap()
                                .entry(file_path.to_string())
                                .or_default()
                                .push(full_symbol.clone());
                            *nodes_count += 1;
                        }
                    }
                }
            } else if ((scala_or_kotlin && Self::declares_fun_or_def(trimmed))
                || trimmed.starts_with("public ")
                || trimmed.starts_with("private ")
                || trimmed.starts_with("protected ")
                || trimmed.starts_with("static ")
                || trimmed.starts_with("default ")
                || trimmed.starts_with("@Test")
                || trimmed.starts_with("@Override")
                || trimmed.starts_with("@ParameterizedTest")
                || trimmed.starts_with("@BeforeEach")
                || trimmed.starts_with("@AfterEach")
                || trimmed.starts_with("void ")
                || trimmed.starts_with("boolean ")
                || trimmed.starts_with("int ")
                || trimmed.starts_with("long ")
                || trimmed.starts_with("double ")
                || trimmed.starts_with("float ")
                || trimmed.starts_with("byte ")
                || trimmed.starts_with("short ")
                || trimmed.starts_with("char ")
                || trimmed.starts_with("String ")
                || trimmed.starts_with("CompletableFuture"))
                && trimmed.contains('(')
            {
                let mut full_sig = trimmed.to_string();
                let is_annotated_test = full_sig.contains("@Test")
                    || (i > 0 && lines[i - 1].trim().starts_with("@Test"));

                // A wrapped parameter list once dropped a method entirely, so
                // the signature is joined until the list closes.
                while !full_sig.contains(')') && i + 1 < lines.len() {
                    i += 1;
                    full_sig.push(' ');
                    full_sig.push_str(lines[i].trim());
                }

                let signature_clean = full_sig.split('{').next().unwrap_or(&full_sig).trim();
                let sig_tokens: Vec<&str> = signature_clean
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();

                let method_name = signature_clean
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim();

                let enclosing_class = match class_stack.last().map(|(c, _)| c.as_str()) {
                    Some(class) => class,
                    // Only Kotlin and Scala reach the fallback: for Java an
                    // empty owner still means the line was not a method, and
                    // inventing one would resurrect the symbols this parser used
                    // to produce from `new Foo(...)` and `catch` clauses.
                    None if scala_or_kotlin => file_stem.as_str(),
                    None => "",
                };
                let is_valid_name = Self::is_valid_identifier(method_name)
                    && !Self::is_java_keyword(method_name)
                    && (method_name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_lowercase() || c == '_' || c == '$')
                        || (!enclosing_class.is_empty() && enclosing_class == method_name));

                if !enclosing_class.is_empty() && is_valid_name {
                    // Record return type for accessor resolution
                    if sig_tokens.len() >= 2 {
                        let raw_ret = sig_tokens[sig_tokens.len() - 2];
                        let ret_clean = raw_ret
                            .split('<')
                            .next_back()
                            .unwrap_or(raw_ret)
                            .replace('>', "")
                            .replace("[]", "");
                        let ret_ident = ret_clean.trim();
                        if Self::is_valid_identifier(ret_ident)
                            && ret_ident.chars().next().is_some_and(|c| c.is_uppercase())
                        {
                            let mut mrt = self.method_return_types.write().unwrap();
                            mrt.insert(method_name.to_string(), ret_ident.to_string());
                            if !package.is_empty() {
                                mrt.insert(
                                    format!("{}.{}", package, method_name),
                                    ret_ident.to_string(),
                                );
                            }
                        }
                    }

                    let full_symbol = if !package.is_empty() {
                        format!("{}.{}::{}", package, enclosing_class, method_name)
                    } else {
                        format!("{}::{}", enclosing_class, method_name)
                    };

                    let is_test_method =
                        is_annotated_test || (is_test_file && method_name.starts_with("test"));
                    let kind = if is_test_method { "test" } else { "method" };

                    let mut node_deps = imports.clone();
                    for ref_t in &referenced_types {
                        if !node_deps.contains(ref_t) && ref_t != &full_symbol {
                            node_deps.push(ref_t.clone());
                        }
                    }

                    self.index_node(&full_symbol, kind, signature_clean, node_deps);
                    self.file_to_symbols
                        .write()
                        .unwrap()
                        .entry(file_path.to_string())
                        .or_default()
                        .push(full_symbol.clone());
                    *nodes_count += 1;
                }
            }

            // Count braces on the current line (or the last line of a multiline signature)
            let curr_line = lines[i];
            let open_count = curr_line.chars().filter(|&c| c == '{').count();
            let close_count = curr_line.chars().filter(|&c| c == '}').count();

            current_brace_depth += open_count;
            current_brace_depth = current_brace_depth.saturating_sub(close_count);

            while let Some((_, depth)) = class_stack.last() {
                if current_brace_depth < *depth {
                    class_stack.pop();
                } else {
                    break;
                }
            }

            i += 1;
        }
    }

    fn parse_rust_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut uses = Vec::new();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("*/")
            {
                continue;
            }

            if trimmed.starts_with("use ") {
                uses.push(
                    trimmed
                        .replace("use ", "")
                        .replace(';', "")
                        .trim()
                        .to_string(),
                );
            } else if trimmed.contains("fn ")
                && (trimmed.starts_with("fn ")
                    || trimmed.starts_with("pub ")
                    || trimmed.starts_with("async ")
                    || trimmed.starts_with("pub async ")
                    || trimmed.starts_with("pub(crate) "))
            {
                let name = trimmed
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .split("fn ")
                    .last()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("test_") || trimmed.contains("#[test]");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(&symbol, kind, trimmed, uses.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            } else if trimmed.starts_with("struct ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("pub(crate) struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("pub enum ")
            {
                let name = trimmed
                    .split_whitespace()
                    .nth(if trimmed.starts_with("pub ") { 2 } else { 1 })
                    .unwrap_or("")
                    .replace(['{', ';'], "")
                    .trim()
                    .to_string();

                if Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    self.index_node_at(&symbol, "struct", trimmed, uses.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_python_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();
        let mut current_class = String::new();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("\"\"\"")
                || trimmed.starts_with("'''")
            {
                continue;
            }

            if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                imports.push(trimmed.to_string());
            } else if trimmed.starts_with("class ") {
                let name = trimmed
                    .replace("class ", "")
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .replace(':', "")
                    .trim()
                    .to_string();
                if Self::is_valid_identifier(&name) {
                    current_class = name.clone();
                    let symbol = format!("{}::{}", file_path, name);
                    let kind = if name.contains("Test") {
                        "test"
                    } else {
                        "class"
                    };
                    self.index_node_at(&symbol, kind, trimmed, imports.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            } else if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                let name = trimmed
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .replace("async def ", "")
                    .replace("def ", "")
                    .trim()
                    .to_string();

                if !name.is_empty() {
                    let symbol = if !current_class.is_empty() {
                        format!("{}::{}::{}", file_path, current_class, name)
                    } else {
                        format!("{}::{}", file_path, name)
                    };
                    let is_test = name.starts_with("test_");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(&symbol, kind, trimmed, imports.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_ts_js_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("import ") {
                imports.push(trimmed.to_string());
            } else if trimmed.contains("function ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export async function ")
            {
                let name = trimmed
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .split("function ")
                    .last()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if !name.is_empty() {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("test")
                        || file_path.contains("test")
                        || file_path.contains("spec");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(&symbol, kind, trimmed, imports.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            } else if trimmed.starts_with("class ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("export default class ")
            {
                let name = trimmed
                    .split("class ")
                    .last()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .replace("{", "")
                    .trim()
                    .to_string();

                if !name.is_empty() {
                    let symbol = format!("{}::{}", file_path, name);
                    self.index_node_at(&symbol, "class", trimmed, imports.clone(), Some(line_no));
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_go_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("func ") {
                let sig = trimmed.replace("func ", "");
                let name = sig.split('(').next().unwrap_or("").trim().to_string();
                if !name.is_empty() {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("Test");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(&symbol, kind, trimmed, vec![], Some(line_no));
                    *nodes_count += 1;
                }
            }
        }
    }

    /// Identifiers appearing immediately before an opening parenthesis, i.e. the
    /// methods a file calls. Derived from already comment- and string-stripped
    /// source, so a name mentioned only in prose never reaches this set.
    fn extract_call_names(clean_code: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let bytes = clean_code.as_bytes();
        let mut start: Option<usize> = None;

        for (i, &b) in bytes.iter().enumerate() {
            let c = b as char;
            if c.is_alphanumeric() || c == '_' || c == '$' {
                if start.is_none() {
                    start = Some(i);
                }
                continue;
            }

            if let Some(s0) = start.take() {
                if c == '(' {
                    let name = &clean_code[s0..i];
                    if !name.is_empty()
                        && !name.chars().next().unwrap().is_ascii_digit()
                        && !names.iter().any(|n| n == name)
                    {
                        names.push(name.to_string());
                    }
                }
            }
        }

        names
    }

    /// Parse an index file.
    ///
    /// The current format carries the resolution side tables alongside the
    /// nodes. Indexes written before those tables existed are a bare node map;
    /// they still load, only without accessor resolution until the next scan.
    fn load_payload(path: &Path) -> std::io::Result<PersistedIndex> {
        let content = std::fs::read_to_string(path)?;
        match serde_json::from_str::<PersistedIndex>(&content) {
            Ok(p) => Ok(p),
            Err(struct_err) => {
                let nodes: HashMap<String, AstNode> =
                    serde_json::from_str(&content).map_err(|bare_err| {
                        std::io::Error::other(
                            format!(
                                "{path:?} parses as neither the current index format ({struct_err}) nor a legacy bare node map ({bare_err})"
                            ),
                        )
                    })?;
                Ok(PersistedIndex {
                    format_version: 1,
                    nodes,
                    method_return_types: HashMap::new(),
                    file_call_names: HashMap::new(),
                    file_to_symbols: HashMap::new(),
                })
            }
        }
    }

    /// Record one symbol into the index on disk without overwriting anything
    /// else in it.
    ///
    /// A mutation is a change to one node. Persisting it by writing this
    /// process's entire in-memory index would also write back every other symbol
    /// as this process last saw it, undoing whatever another agent recorded in
    /// the meantime. So the current file is re-read under the lock, the one node
    /// is inserted, and the result is written atomically.
    pub fn persist_symbol(&self, file_path: &Path, symbol: &str) -> std::io::Result<PathBuf> {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            std::env::current_dir()?.join(file_path)
        };
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| explain_denied(parent, e))?;
        }

        let node = self.get_symbol(symbol).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{symbol:?} is not indexed"),
            )
        })?;

        let _lock = IndexLock::acquire(&abs_path).map_err(|e| explain_denied(&abs_path, e))?;

        let mut payload = match Self::load_payload(&abs_path) {
            Ok(p) => p,
            Err(_) => PersistedIndex {
                format_version: INDEX_FORMAT_VERSION,
                nodes: HashMap::new(),
                method_return_types: HashMap::new(),
                file_call_names: HashMap::new(),
                file_to_symbols: HashMap::new(),
            },
        };

        payload.format_version = INDEX_FORMAT_VERSION;
        payload.nodes.insert(symbol.to_string(), node);

        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        write_atomically(&abs_path, json.as_bytes()).map_err(|e| explain_denied(&abs_path, e))?;
        Ok(abs_path)
    }

    /// Persist the Merkle AST CAS index to disk (.axiom/index.json)
    pub fn save_to_disk(&self, file_path: &Path) -> std::io::Result<PathBuf> {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            std::env::current_dir()?.join(file_path)
        };

        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| explain_denied(parent, e))?;
        }

        let _lock = IndexLock::acquire(&abs_path).map_err(|e| explain_denied(&abs_path, e))?;

        // Merge over whatever is on disk now rather than replacing it. Another
        // agent may have recorded a symbol since this process loaded, and
        // writing this view whole would take that symbol with it. Removals this
        // process made are subtracted explicitly, so a re-scan still drops what
        // it purged instead of a union resurrecting it.
        let mut payload = Self::load_payload(&abs_path).unwrap_or(PersistedIndex {
            format_version: INDEX_FORMAT_VERSION,
            nodes: HashMap::new(),
            method_return_types: HashMap::new(),
            file_call_names: HashMap::new(),
            file_to_symbols: HashMap::new(),
        });

        for symbol in self.forgotten_symbols.read().unwrap().iter() {
            payload.nodes.remove(symbol);
        }
        for file in self.forgotten_files.read().unwrap().iter() {
            payload.file_call_names.remove(file);
            payload.file_to_symbols.remove(file);
        }

        payload.format_version = INDEX_FORMAT_VERSION;
        payload.nodes.extend(self.nodes.read().unwrap().clone());
        payload
            .method_return_types
            .extend(self.method_return_types.read().unwrap().clone());
        payload
            .file_call_names
            .extend(self.file_call_names.read().unwrap().clone());
        payload
            .file_to_symbols
            .extend(self.file_to_symbols.read().unwrap().clone());

        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        write_atomically(&abs_path, json.as_bytes()).map_err(|e| explain_denied(&abs_path, e))?;

        // Recorded so the next save does not have to re-apply them.
        self.forgotten_symbols.write().unwrap().clear();
        self.forgotten_files.write().unwrap().clear();

        // Verify write succeeded
        if !abs_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Failed to verify write to {:?}", abs_path),
            ));
        }

        Ok(abs_path)
    }

    /// Load existing Merkle AST CAS index from disk
    pub fn load_from_disk(file_path: &Path) -> std::io::Result<Self> {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            std::env::current_dir()?.join(file_path)
        };

        let payload = Self::load_payload(&abs_path)?;

        let mut reverse_deps = HashMap::new();
        for (symbol, node) in &payload.nodes {
            for dep in &node.dependencies {
                reverse_deps
                    .entry(dep.clone())
                    .or_insert_with(Vec::new)
                    .push(symbol.clone());
            }
        }

        // The searchable text is not stored in the index: it would duplicate the
        // working tree and go stale against it. The scan recorded which files it
        // read, so re-read them here. Files that have since moved or been deleted
        // are skipped, which costs their text search rather than the whole load.
        let mut zoekt = ZoektIndex::new();
        for file_path in payload.file_call_names.keys() {
            if let Ok(text) = std::fs::read_to_string(file_path) {
                zoekt.add_document(file_path, &text);
            }
        }

        Ok(Self {
            nodes: RwLock::new(payload.nodes),
            reverse_deps: RwLock::new(reverse_deps),
            cas_cache: RwLock::new(HashMap::new()),
            zoekt_index: RwLock::new(zoekt),
            method_return_types: RwLock::new(payload.method_return_types),
            file_call_names: RwLock::new(payload.file_call_names),
            file_to_symbols: RwLock::new(payload.file_to_symbols),
            forgotten_symbols: RwLock::new(HashSet::new()),
            forgotten_files: RwLock::new(HashSet::new()),
            parsing_file: RwLock::new(None),
            // Both are scan-scoped. A loaded index already carries the edges
            // they were used to produce, in the nodes' own dependencies.
            symbol_lines: RwLock::new(HashMap::new()),
            pending_refs: RwLock::new(HashMap::new()),
        })
    }
}

/// Zoekt Trigram-based In-Memory Search Engine
pub struct ZoektIndex {
    files: HashMap<String, String>,
    trigrams: HashMap<[u8; 3], HashSet<String>>,
}

impl Default for ZoektIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoektIndex {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            trigrams: HashMap::new(),
        }
    }

    pub fn add_document(&mut self, path: &str, content: &str) {
        self.files.insert(path.to_string(), content.to_string());
        let bytes = content.as_bytes();
        if bytes.len() >= 3 {
            for i in 0..bytes.len() - 2 {
                let tri = [bytes[i], bytes[i + 1], bytes[i + 2]];
                self.trigrams
                    .entry(tri)
                    .or_default()
                    .insert(path.to_string());
            }
        }
    }

    pub fn search(
        &self,
        query: &str,
        compiled: Option<&Regex>,
        max_results: usize,
    ) -> Vec<ZoektMatch> {
        let mut matches = Vec::new();
        let query_bytes = query.as_bytes();

        // Trigram prefiltering only holds for a literal query: the trigrams of a
        // pattern are not text that appears in any file. A regex search scans
        // every document instead, trading speed for not missing matches.
        let candidates: Vec<&String> = if compiled.is_some() {
            self.files.keys().collect()
        } else if query_bytes.len() >= 3 {
            let mut candidate_set: Option<HashSet<String>> = None;
            for i in 0..query_bytes.len() - 2 {
                let tri = [query_bytes[i], query_bytes[i + 1], query_bytes[i + 2]];
                if let Some(set) = self.trigrams.get(&tri) {
                    if let Some(ref mut c) = candidate_set {
                        *c = c.intersection(set).cloned().collect();
                    } else {
                        candidate_set = Some(set.clone());
                    }
                }
            }
            if let Some(c) = candidate_set {
                self.files.keys().filter(|k| c.contains(*k)).collect()
            } else {
                self.files.keys().collect()
            }
        } else {
            self.files.keys().collect()
        };

        for path in candidates {
            if let Some(content) = self.files.get(path) {
                for (line_no, line) in content.lines().enumerate() {
                    let hit = match compiled {
                        Some(re) => re.is_match(line),
                        None => line.contains(query),
                    };
                    if hit {
                        matches.push(ZoektMatch {
                            match_kind: "text".to_string(),
                            file_path: path.clone(),
                            line_number: Some(line_no + 1),
                            line_content: line.trim().to_string(),
                        });
                        if matches.len() >= max_results {
                            return matches;
                        }
                    }
                }
            }
        }

        matches
    }
}

/// How a search query is interpreted.
///
/// The default is deliberately `Literal`. Real queries an agent sends are full
/// of regex metacharacters that it means literally: `validate_token(`,
/// `List<String>`, `config.threads`. Guessing regex from the presence of those
/// characters would silently answer a different question than the one asked,
/// so regex is opt-in and the mode actually used is reported back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Match the query as plain text.
    Literal,
    /// Compile the query as a regular expression.
    Regex,
    /// Use regex only if the query both parses as one and contains a construct
    /// that is meaningless as literal text. Never silently reinterprets a query
    /// that could plausibly be literal.
    Auto,
}

impl SearchMode {
    /// Parse a caller-supplied mode. Unknown values are rejected rather than
    /// quietly treated as the default.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "literal" | "" => Ok(SearchMode::Literal),
            "regex" => Ok(SearchMode::Regex),
            "auto" => Ok(SearchMode::Auto),
            other => Err(format!(
                "unknown search mode {:?}; expected \"literal\", \"regex\" or \"auto\"",
                other
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Literal => "literal",
            SearchMode::Regex => "regex",
            SearchMode::Auto => "auto",
        }
    }
}

/// Constructs that carry no meaning as literal source text, so a query holding
/// one was almost certainly written as a pattern. Bare `.`, `(`, `)`, `<` and
/// `>` are excluded on purpose: they are ordinary code punctuation.
fn looks_like_a_pattern(query: &str) -> bool {
    const PATTERN_TOKENS: [&str; 10] = [".*", ".+", "[", "]", "^", "$", "\\b", "\\d", "\\w", "\\s"];
    PATTERN_TOKENS.iter().any(|t| query.contains(t))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZoektMatch {
    /// "text" for a hit on a line of source, "symbol" for a hit on a symbol name.
    /// A symbol hit has no line to point at, so `line_number` is None there.
    pub match_kind: String,
    pub file_path: String,
    pub line_number: Option<usize>,
    pub line_content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScanSummary {
    pub files_scanned: usize,
    pub nodes_indexed: usize,
    pub total_symbols: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlastRadiusResult {
    pub symbol: String,
    pub impacted_tests: Vec<String>,
    pub direct_tests: Vec<String>,
    pub tests_by_depth: HashMap<usize, Vec<String>>,
    pub total_tests_in_repo: usize,
    pub pruned_test_percentage: f64,
}

/// A symbol's forward dependency closure: everything it can reach.
///
/// The blast radius walks `reverse_deps`, from a changed symbol out to the tests
/// that reach it. A verdict cache needs the other direction: given a test, what
/// does its outcome depend on? If that set is unchanged since the last run, the
/// previous verdict is still valid and neither the test nor the compilation
/// behind it has to happen again.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForwardClosure {
    pub symbol: String,
    /// Indexed symbols reachable from `symbol`, including itself. Sorted, so the
    /// hash over them does not depend on traversal order.
    pub reachable: Vec<String>,
    /// Names several indexed symbols answer to, with how many, where every
    /// candidate was taken rather than one of them chosen.
    ///
    /// The direction of the guess is what matters, and it runs opposite to the
    /// blast radius. There, a wrong extra edge costs one unnecessary test run.
    /// Here, a missing edge means a test is skipped on the strength of a key
    /// that did not cover the thing that changed, and a stale pass is reported
    /// for code that never ran.
    ///
    /// So an ambiguous name is over-approximated: the closure depends on every
    /// symbol that could answer to it. The real target is among them whenever it
    /// is in the index at all, so nothing is missed. What it costs is precision,
    /// and that cost is recorded here rather than hidden: editing any same-named
    /// symbol invalidates the key, and these counts say how much of the tree
    /// each name drags in.
    ///
    /// Picking the nearest candidate instead, by file or by directory, was the
    /// obvious alternative and is unsafe for exactly this reason. A wrong pick
    /// produces a key that looks complete and omits the dependency that moved.
    pub over_approximated: Vec<(String, usize)>,
    /// Names no indexed symbol answers to, which on a real tree means a crate
    /// outside it: `anyhow::Result`, `std::path::{Path, PathBuf}`.
    ///
    /// These do not block a key. The index was never going to hold them, and
    /// their contents cannot change between two runs on one machine unless the
    /// toolchain or a lock file changes, which [`EnvironmentKey`] covers. Before
    /// they were separated out, every test importing anything was unkeyable,
    /// which was every test.
    ///
    /// They are still recorded, and still hashed, because *which* outside names
    /// a test reaches is itself an input: adding an import changes what the test
    /// does even when nothing inside the tree moved.
    pub outside: Vec<String>,
}

/// What an indexed lookup of a dependency name found.
///
/// Three answers rather than two. Collapsing the last two into `None` is what
/// made every closure incomplete: a crate outside the tree and a name this tree
/// defines several times are both unresolved, and only one of them is a gap in
/// the graph.
enum Resolution {
    One(String),
    /// Several indexed symbols answer to the name and nothing here can say
    /// which was meant. Every one of them is taken, not one of them chosen.
    Several(Vec<String>),
    /// Nothing in the index matches. Folded into the environment key instead.
    Outside,
}

impl ForwardClosure {
    /// True when every name resolved to exactly one symbol.
    ///
    /// Informational, not a gate. An imprecise closure is still safe to key on,
    /// because over-approximation cannot miss a dependency; it just invalidates
    /// more often than it needs to. What decides whether a key may be issued is
    /// in [`AstIndex::closure_hash`], and it is about the environment rather
    /// than about this.
    pub fn is_precise(&self) -> bool {
        self.over_approximated.is_empty()
    }

    /// How many symbols this closure drags in per name it could not pin down.
    ///
    /// A closure that over-approximates a handful of names is cheap. One that
    /// over-approximates a name matching half the index is a key that changes
    /// whenever anything does, which is a cache that never hits. The number is
    /// reported so that failure mode is visible as a number rather than as
    /// unexplained misses.
    pub fn over_approximation_cost(&self) -> usize {
        self.over_approximated.iter().map(|(_, n)| n).sum()
    }
}

/// What a verdict cache would have decided, measured against what the blast
/// radius selects, without skipping anything.
///
/// Shadow mode: this computes the decision and reports it. Nothing is cached and
/// no test is skipped, because the number that decides whether a cache is safe
/// to build is how often it would have been wrong, and that has to be measured
/// before it is relied on rather than after.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CacheAudit {
    pub symbols_audited: usize,
    pub tests_in_index: usize,
    /// Tests every one of whose names resolved to exactly one symbol.
    ///
    /// Not a gate. A test that over-approximates is still keyable, because
    /// taking every candidate cannot miss the real one. This counts how often
    /// the graph was precise enough not to need that.
    pub tests_with_precise_closure: usize,
    /// Tests that can be keyed at all, which is the number a cache would live
    /// on. A key is refused only when a name from outside the tree is reached
    /// and the environment covers nothing.
    pub tests_with_a_key: usize,
    /// Total extra symbols pulled in by over-approximation, across all tests.
    ///
    /// The cost side of the safety. A closure that drags in half the index for
    /// one unpinnable name is a key that changes whenever anything does, so
    /// this is the number that says whether the cache would ever hit.
    pub over_approximation_cost: usize,
    /// Symbol/test pairs where both mechanisms agree the test is affected.
    pub agreements: usize,
    /// The dangerous disagreement: the blast radius selects the test, and the
    /// test's forward closure does not contain the symbol. A cache keyed on that
    /// closure would skip a test the selector says must run.
    pub would_wrongly_skip: usize,
    /// The safe disagreement: the closure contains the symbol but the blast
    /// radius did not select the test. Costs a test run, and points at
    /// under-selection rather than at an unsound cache.
    pub would_run_unselected: usize,
    /// Up to this many examples of the dangerous case, for a report to name.
    pub wrongly_skipped_examples: Vec<(String, String)>,
    /// Summed over every symbol: how many tests have a key that moves when that
    /// symbol changes.
    ///
    /// This is the cache's own answer to "what has to run", ignoring the
    /// selector. Divided by `symbols_audited` it gives the mean size of a
    /// cache-driven test run for a one-symbol change, and that is the number
    /// that says whether a cache would be worth having at all. Nothing else
    /// reported here measures usefulness; the rest measure safety.
    pub tests_whose_key_moves: usize,
    /// The names that most often blocked a key, with counts. These are the gaps
    /// in the graph: something in this tree satisfies them and the graph cannot
    /// say what.
    pub top_ambiguous: Vec<(String, usize)>,
    /// The out-of-tree names most often reached, with counts. Reported because
    /// they are what [`EnvironmentKey`] has to cover, not because they are
    /// defects.
    pub top_outside: Vec<(String, usize)>,
}

impl CacheAudit {
    /// How many tests a cache-driven run would contain, for a change to one
    /// symbol, on average. `None` when nothing was audited.
    pub fn mean_tests_per_cache_run(&self) -> Option<f64> {
        if self.symbols_audited == 0 {
            return None;
        }
        Some(self.tests_whose_key_moves as f64 / self.symbols_audited as f64)
    }

    /// The same for the selector, so the two can be compared directly.
    ///
    /// A test the blast radius selects is one it believes the change can reach,
    /// which is `agreements + would_wrongly_skip`: the pairs where both agree,
    /// plus the pairs the selector claims and the closure does not.
    pub fn mean_tests_per_selected_run(&self) -> Option<f64> {
        if self.symbols_audited == 0 {
            return None;
        }
        let selected = self.agreements + self.would_wrongly_skip;
        Some(selected as f64 / self.symbols_audited as f64)
    }

    /// Tests a cache would skip that the selector would have run.
    ///
    /// Layered behind blast-radius selection, a test runs when the selector
    /// picks it *and* its key moved, so the work the cache removes is exactly
    /// the pairs where the selector picks it and the key did not move. That is
    /// `would_wrongly_skip`, the same number the safety argument turns on.
    ///
    /// Which makes the two readings inseparable: on this design a cache saves
    /// nothing behind the selector unless it disagrees with it, and every
    /// disagreement is a test the selector says must run. A zero here is both
    /// "provably safe" and "provably pointless", and reporting one without the
    /// other is how a feature gets built on half a number.
    pub fn tests_saved_behind_the_selector(&self) -> usize {
        self.would_wrongly_skip
    }

    /// Share of decisions the two mechanisms agreed on, or `None` when there
    /// were no decisions to make. Reported rather than inferred from the counts,
    /// so a zero-denominator run cannot read as 100%.
    pub fn agreement_rate(&self) -> Option<f64> {
        let total = self.agreements + self.would_wrongly_skip + self.would_run_unselected;
        if total == 0 {
            return None;
        }
        Some(self.agreements as f64 / total as f64)
    }
}

impl AstIndex {
    /// How deep the forward walk goes. The reverse walk bounds itself the same
    /// way; an unbounded closure on a cyclic graph is the whole index, which
    /// tells a cache nothing.
    pub const CLOSURE_DEPTH: usize = 5;

    /// Every indexed symbol reachable from `symbol_path` by following
    /// dependencies, plus the names that resolved to nothing.
    pub fn forward_closure(&self, symbol_path: &str, max_depth: usize) -> Option<ForwardClosure> {
        let start = self.get_symbol(symbol_path)?;
        let nodes = self.nodes.read().unwrap();

        let mut reachable: BTreeSet<String> = BTreeSet::new();
        let mut over_approximated: BTreeMap<String, usize> = BTreeMap::new();
        let mut outside: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        reachable.insert(start.symbol_path.clone());
        queue.push_back((start.symbol_path.clone(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let Some(node) = nodes.get(&current) else {
                continue;
            };

            // A method depends on the type that encloses it. Containment is not
            // a call, so nothing recorded it as an edge, and the closure of
            // `BillingTest::test_total` did not contain `BillingTest`. The blast
            // radius selects that test when the class changes, correctly, so the
            // two mechanisms disagreed and a cache keyed on the closure would
            // have skipped a test whose class had moved. Found by running the
            // audit on a second tree, not on this one.
            //
            // Only the immediate owner is added here; a nested type reaches its
            // own owner when it comes off the queue.
            if let Some((owner, _)) = current.rsplit_once("::") {
                if nodes.contains_key(owner) && reachable.insert(owner.to_string()) {
                    queue.push_back((owner.to_string(), depth + 1));
                }
            }

            for dependency in &node.dependencies {
                let name = dependency.trim();
                if name.is_empty() {
                    continue;
                }
                // Resolution goes through the same lookup a caller would use, or
                // the closure would cover symbols the rest of the system cannot
                // reach by that name. An ambiguous name resolves to nothing,
                // which makes the closure incomplete rather than picking one.
                match Self::resolve_within(&nodes, name) {
                    Resolution::One(resolved) => {
                        if reachable.insert(resolved.clone()) {
                            queue.push_back((resolved, depth + 1));
                        }
                    }
                    Resolution::Several(candidates) => {
                        // Every candidate, not the likeliest one. The real
                        // target is among them, so the closure cannot miss it.
                        over_approximated.insert(name.to_string(), candidates.len());
                        for candidate in candidates {
                            if reachable.insert(candidate.clone()) {
                                queue.push_back((candidate, depth + 1));
                            }
                        }
                    }
                    Resolution::Outside => {
                        outside.insert(name.to_string());
                    }
                }
            }
        }

        Some(ForwardClosure {
            symbol: start.symbol_path,
            reachable: reachable.into_iter().collect(),
            over_approximated: over_approximated.into_iter().collect(),
            outside: outside.into_iter().collect(),
        })
    }

    /// Resolve a dependency name against an already-held read guard.
    ///
    /// `get_symbol` takes the lock itself, and the closure walk holds it for the
    /// whole traversal, so the lookup is repeated here rather than dropping and
    /// retaking the guard once per edge.
    /// The remainder of a path that names something inside this tree, if it is
    /// one.
    ///
    /// Rust spells an in-crate path `crate::a::b`, and inside a module also
    /// `self::b` and `super::b`. All three are statements that the target is
    /// here, which is exactly what the closure needs to know, and none of them
    /// can be a crate from outside.
    fn strip_in_crate_prefix(name: &str) -> Option<&str> {
        for prefix in ["crate::", "self::", "super::"] {
            if let Some(rest) = name.strip_prefix(prefix) {
                // `super::super::x` unwinds to `x` rather than stopping at the
                // second `super`.
                return Some(Self::strip_in_crate_prefix(rest).unwrap_or(rest));
            }
        }
        None
    }

    fn resolve_within(nodes: &HashMap<String, AstNode>, name: &str) -> Resolution {
        if nodes.contains_key(name) {
            return Resolution::One(name.to_string());
        }
        let mut matches: Vec<String> = nodes
            .keys()
            .filter(|k| Self::is_suffix_match(k, name))
            .cloned()
            .collect();

        // A `crate::`, `self::` or `super::` path names something in this tree
        // by definition, so failing to match one must not be read as "outside".
        //
        // `crate::auth::validate_token` did exactly that: it matched no key,
        // because the key is the file path plus the symbol, so it was filed as a
        // crate outside the tree and folded into the environment digest. An
        // in-tree dependency covered by the environment key is the unsound
        // direction, since editing it would not move the key. It was harmless
        // only where the same test also called the function by its bare name.
        //
        // The fallback is restricted to these three prefixes on purpose. Doing
        // it for any dotted or colonned name would let `anyhow::Result` match a
        // local `Result` and stop being covered by the environment, which is the
        // same unsoundness pointing the other way.
        if matches.is_empty() {
            if let Some(rest) = Self::strip_in_crate_prefix(name) {
                let tail = rest.rsplit("::").next().unwrap_or(rest);
                matches = nodes
                    .keys()
                    .filter(|k| Self::is_suffix_match(k, rest) || Self::is_suffix_match(k, tail))
                    .cloned()
                    .collect();
            }
        }

        match matches.len() {
            // Nothing in the index answers to this name at all, which on a real
            // tree usually means a crate outside it.
            0 => Resolution::Outside,
            1 => Resolution::One(matches.pop().expect("length checked")),
            // Sorted so the closure, and therefore the key, does not depend on
            // the order a HashMap happened to yield.
            _ => {
                matches.sort();
                Resolution::Several(matches)
            }
        }
    }
}

impl AstIndex {
    /// A digest over everything a symbol's outcome depends on, or `None` when
    /// the closure is incomplete.
    ///
    /// `None` is the point of the return type. A cache that answers with a hash
    /// whatever it knows will key on a partial view and skip a test whose real
    /// dependency changed behind an unresolved name. Refusing to produce a key
    /// is what makes the miss happen.
    pub fn closure_hash(&self, symbol_path: &str, environment: &EnvironmentKey) -> Option<String> {
        let closure = self.forward_closure(symbol_path, Self::CLOSURE_DEPTH)?;

        // The one thing that still refuses a key. An ambiguous name is safe,
        // because every candidate was taken; a name from outside the tree is
        // safe only while something pins what it means. When the environment
        // covers nothing, nothing does, and a key issued here would stay stable
        // across the dependency upgrade that changed the answer.
        if !closure.outside.is_empty() && environment.covers_nothing() {
            return None;
        }

        let nodes = self.nodes.read().unwrap();
        let mut hasher = blake3::Hasher::new();

        // The environment first, so a lock file or compiler change moves every
        // key at once rather than only the keys of tests that happen to import
        // something.
        hasher.update(environment.as_str().as_bytes());
        hasher.update(
            b"
",
        );

        // Which out-of-tree names the test reaches is itself an input: adding an
        // import changes what the test does even when nothing inside the tree
        // moved. What is *behind* those names is the environment key's job.
        for name in &closure.outside {
            hasher.update(name.as_bytes());
            hasher.update(
                b"
",
            );
        }

        for symbol in &closure.reachable {
            // Both the identity and the content: renaming a symbol changes what
            // a test calls, and editing its body changes what that call does.
            hasher.update(symbol.as_bytes());
            hasher.update(b"\0");
            if let Some(node) = nodes.get(symbol) {
                hasher.update(node.hash.as_bytes());
            }
            hasher.update(b"\n");
        }
        Some(format!("closure_{}", hasher.finalize().to_hex()))
    }

    /// Measure a verdict cache without running one.
    ///
    /// For every symbol, the tests the blast radius selects are compared against
    /// the tests whose forward closure contains that symbol. Where the two
    /// disagree, a cache keyed on the closure would decide differently from the
    /// selector already shipping, and the direction says whether that is
    /// dangerous or merely wasteful.
    pub fn audit_cache(
        &self,
        environment: &EnvironmentKey,
        max_depth: usize,
        example_limit: usize,
    ) -> CacheAudit {
        let (test_symbols, all_symbols) = {
            let nodes = self.nodes.read().unwrap();
            let tests: Vec<String> = nodes
                .values()
                .filter(|n| n.kind == "test")
                .map(|n| n.symbol_path.clone())
                .collect();
            let all: Vec<String> = nodes.keys().cloned().collect();
            (tests, all)
        };

        let mut audit = CacheAudit {
            tests_in_index: test_symbols.len(),
            ..Default::default()
        };

        // One closure per test, computed once and reused across every symbol.
        let mut closures: HashMap<String, HashSet<String>> = HashMap::new();
        let mut ambiguous_counts: HashMap<String, usize> = HashMap::new();
        let mut outside_counts: HashMap<String, usize> = HashMap::new();
        for test in &test_symbols {
            if let Some(closure) = self.forward_closure(test, Self::CLOSURE_DEPTH) {
                if closure.is_precise() {
                    audit.tests_with_precise_closure += 1;
                }
                audit.over_approximation_cost += closure.over_approximation_cost();
                if self.closure_hash(test, environment).is_some() {
                    audit.tests_with_a_key += 1;
                }
                for (name, candidates) in &closure.over_approximated {
                    let entry = ambiguous_counts.entry(name.clone()).or_insert(0);
                    *entry = (*entry).max(*candidates);
                }
                for name in &closure.outside {
                    *outside_counts.entry(name.clone()).or_insert(0) += 1;
                }
                closures.insert(test.clone(), closure.reachable.into_iter().collect());
            }
        }

        // By count, then by name, so two runs over the same tree report the same
        // list rather than whatever order the map happened to yield.
        let rank = |counts: HashMap<String, usize>| {
            let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ranked.truncate(example_limit);
            ranked
        };
        audit.top_ambiguous = rank(ambiguous_counts);
        audit.top_outside = rank(outside_counts);

        for symbol in &all_symbols {
            let Some(radius) = self.compute_blast_radius(symbol, max_depth) else {
                continue;
            };
            audit.symbols_audited += 1;

            let selected: HashSet<&String> = radius.impacted_tests.iter().collect();

            for test in &test_symbols {
                let Some(reachable) = closures.get(test) else {
                    continue;
                };
                let key_moves = reachable.contains(symbol);
                if key_moves {
                    audit.tests_whose_key_moves += 1;
                }
                match (selected.contains(test), key_moves) {
                    (true, true) => audit.agreements += 1,
                    (true, false) => {
                        audit.would_wrongly_skip += 1;
                        if audit.wrongly_skipped_examples.len() < example_limit {
                            audit
                                .wrongly_skipped_examples
                                .push((symbol.clone(), test.clone()));
                        }
                    }
                    (false, true) => audit.would_run_unselected += 1,
                    (false, false) => {}
                }
            }
        }

        audit
    }
}

/// Everything a verdict depends on that is not a symbol in this tree.
///
/// Separating out-of-tree names from ambiguous ones is only safe if something
/// else covers them. This is that something else. A `cargo update` rewrites
/// `Cargo.lock`, a compiler upgrade changes the version strings, and either one
/// changes this digest, which changes every key derived from it and invalidates
/// the whole cache at once. That is the correct blast radius for a change to the
/// environment: nothing that was compiled against the old one is still known to
/// hold.
///
/// It is deliberately coarse. A finer key would invalidate less, and getting it
/// wrong would leave a verdict standing against a dependency that moved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentKey {
    digest: String,
    /// What went into it, so a report can say why a key changed rather than
    /// only that it did.
    pub inputs: Vec<String>,
}

impl EnvironmentKey {
    /// Lock files and manifests that pin what an out-of-tree name resolves to.
    ///
    /// Manifests are included alongside lock files because a language without a
    /// lock file still pins versions somewhere, and a key that covers neither is
    /// worse than one that covers the looser of the two.
    const MANIFESTS: &'static [&'static str] = &[
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "deno.lock",
        "go.sum",
        "poetry.lock",
        "Pipfile.lock",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "rust-toolchain.toml",
        "rust-toolchain",
        ".tool-versions",
    ];

    /// Digest the manifests under `root`, plus any toolchain fingerprints the
    /// caller supplies.
    ///
    /// The fingerprints are passed in rather than probed here because this crate
    /// indexes files and does not run compilers; the tier that already probes
    /// them supplies the strings.
    pub fn of(root: &Path, toolchains: &[String]) -> Self {
        let mut hasher = blake3::Hasher::new();
        let mut inputs: Vec<String> = Vec::new();

        for name in Self::MANIFESTS {
            let candidate = root.join(name);
            let Ok(bytes) = std::fs::read(&candidate) else {
                continue;
            };
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\n");
            inputs.push((*name).to_string());
        }

        // Sorted, so two runs on one machine agree whatever order the caller
        // probed the toolchains in.
        let mut sorted: Vec<&String> = toolchains.iter().collect();
        sorted.sort();
        for fingerprint in sorted {
            hasher.update(fingerprint.as_bytes());
            hasher.update(b"\n");
            inputs.push(fingerprint.clone());
        }

        Self {
            digest: format!("env_{}", hasher.finalize().to_hex()),
            inputs,
        }
    }

    /// A key covering nothing, for a caller that has no environment to describe.
    ///
    /// Named rather than defaulted: a verdict keyed against this is keyed
    /// against no environment at all, and that should be visible at the call
    /// site instead of happening by omission.
    pub fn uncovered() -> Self {
        Self {
            digest: "env_uncovered".to_string(),
            inputs: Vec::new(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.digest
    }

    /// True when nothing was found to cover. A key derived from this is not
    /// wrong, but it says nothing about the environment, and a report should
    /// say so rather than showing a digest that looks like an answer.
    pub fn covers_nothing(&self) -> bool {
        self.inputs.is_empty()
    }
}
