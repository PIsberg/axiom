use axiom_proto::AstNode;
use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
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
            std::fs::create_dir_all(parent)?;
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
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;

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
        }
    }

    /// Insert or update an AST Node into the Merkle index
    pub fn index_node(
        &self,
        symbol: &str,
        kind: &str,
        content: &str,
        deps: Vec<String>,
    ) -> AstNode {
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
    pub fn language_of_symbol(&self, symbol_path: &str) -> Option<String> {
        let file_syms = self.file_to_symbols.read().unwrap();
        for (file, symbols) in file_syms.iter() {
            if symbols.iter().any(|s| s == symbol_path) {
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
        let simple_name = canonical_symbol
            .split('.')
            .next_back()
            .unwrap_or(&canonical_symbol)
            .split("::")
            .next()
            .unwrap_or(&canonical_symbol);

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
                if let Some(callers) = rev.get(&curr) {
                    for caller in callers {
                        if visited.insert(caller.clone()) {
                            queue.push_back((caller.clone(), depth + 1));
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
        self.forgotten_files
            .write()
            .unwrap()
            .insert(file_path.to_string());

        if let Some(symbols) = previous {
            let mut nodes = self.nodes.write().unwrap();
            let mut forgotten = self.forgotten_symbols.write().unwrap();
            for symbol in symbols {
                nodes.remove(&symbol);
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
            } else if (trimmed.contains("class ")
                || trimmed.contains("interface ")
                || trimmed.contains("enum ")
                || trimmed.contains("record "))
                && (trimmed.starts_with("public ")
                    || trimmed.starts_with("private ")
                    || trimmed.starts_with("protected ")
                    || trimmed.starts_with("abstract ")
                    || trimmed.starts_with("final ")
                    || trimmed.starts_with("static ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("interface ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("record ")
                    || trimmed.starts_with("@interface "))
            {
                let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                if let Some(pos) = tokens
                    .iter()
                    .position(|&t| t == "class" || t == "interface" || t == "enum" || t == "record")
                {
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
            } else if (trimmed.starts_with("public ")
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

                let enclosing_class = class_stack.last().map(|(c, _)| c.as_str()).unwrap_or("");
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

        for line in content.lines() {
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

                    self.index_node(&symbol, kind, trimmed, uses.clone());
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
                    self.index_node(&symbol, "struct", trimmed, uses.clone());
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_python_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();
        let mut current_class = String::new();

        for line in content.lines() {
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
                    self.index_node(&symbol, kind, trimmed, imports.clone());
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

                    self.index_node(&symbol, kind, trimmed, imports.clone());
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_ts_js_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();

        for line in content.lines() {
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

                    self.index_node(&symbol, kind, trimmed, imports.clone());
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
                    self.index_node(&symbol, "class", trimmed, imports.clone());
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_go_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("func ") {
                let sig = trimmed.replace("func ", "");
                let name = sig.split('(').next().unwrap_or("").trim().to_string();
                if !name.is_empty() {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("Test");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node(&symbol, kind, trimmed, vec![]);
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
            std::fs::create_dir_all(parent)?;
        }

        let node = self.get_symbol(symbol).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{symbol:?} is not indexed"),
            )
        })?;

        let _lock = IndexLock::acquire(&abs_path)?;

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
        write_atomically(&abs_path, json.as_bytes())?;
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
            std::fs::create_dir_all(parent)?;
        }

        let _lock = IndexLock::acquire(&abs_path)?;

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

        write_atomically(&abs_path, json.as_bytes())?;

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
