use axiom_proto::AstNode;
use rayon::prelude::*;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

thread_local! {
    /// The file the calling thread is currently parsing, so `index_node_at`
    /// attributes a symbol to it whichever parser produced the symbol.
    ///
    /// A thread-local rather than a field, because the walk parses files in
    /// parallel: a single shared "current file" would be whatever file some
    /// other worker happened to set last, so a symbol from one file would be
    /// filed under another. Each worker sets its own for the duration of one
    /// file.
    static PARSING_FILE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Ingesting a precise SCIP index in place of the heuristic parsers.
pub mod scip_ingest;

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
    /// Set on drop to stop the heartbeat, which then exits at its next tick.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The heartbeat thread, joined on drop so it cannot touch the lock file
    /// after the lock is released.
    heartbeat: Option<std::thread::JoinHandle<()>>,
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

    /// How often a held lock refreshes its own mtime, well under `STALE_AFTER`
    /// so several refreshes fall inside one staleness window.
    ///
    /// Without this a write that runs longer than `STALE_AFTER`, a large index
    /// save under contention, left the lock untouched, so a live writer looked
    /// like a crashed one and another agent took the lock over mid-write. The
    /// heartbeat means an aged mtime is evidence the holder is really gone.
    const HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(500);

    /// How long to keep waiting for a live holder before giving up.
    const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

    /// Start the heartbeat that keeps a held lock's mtime fresh. It refreshes
    /// only while the file still holds our token: if another agent judged us
    /// stale and took over, the heartbeat stops rather than stamping on their
    /// lock.
    fn start_heartbeat(
        path: PathBuf,
        token: String,
    ) -> (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            loop {
                // Sleep in small steps so a drop is noticed promptly rather than
                // after a full interval.
                let mut waited = std::time::Duration::ZERO;
                while waited < Self::HEARTBEAT {
                    if stop_thread.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    waited += std::time::Duration::from_millis(50);
                }
                if stop_thread.load(Ordering::Relaxed) {
                    return;
                }
                // Refresh only if the lock is still ours. Rewriting the token
                // updates the mtime; if the file is gone or holds someone
                // else's token, stop.
                match std::fs::read_to_string(&path) {
                    Ok(found) if found == token => {
                        let _ = std::fs::write(&path, token.as_bytes());
                    }
                    _ => return,
                }
            }
        });
        (stop, handle)
    }

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
                        Ok(found) if found == token => {
                            let (stop, handle) = Self::start_heartbeat(path.clone(), token.clone());
                            return Ok(Self {
                                path,
                                token,
                                stop,
                                heartbeat: Some(handle),
                            });
                        }
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
                                Self::GIVE_UP_AFTER,
                                path
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
        // Stop the heartbeat and wait for it to exit before touching the file,
        // or a late refresh could recreate the lock just after it is removed.
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.heartbeat.take() {
            let _ = handle.join();
        }

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
    #[serde(default)]
    type_hierarchy: HashMap<String, Vec<String>>,
    #[serde(default)]
    interface_implementors: HashMap<String, Vec<String>>,
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
    /// Type inheritance hierarchy (child type -> parent types / interfaces)
    type_hierarchy: RwLock<HashMap<String, Vec<String>>>,
    /// Interface & superclass implementors (parent type -> implementing child types)
    interface_implementors: RwLock<HashMap<String, Vec<String>>>,
    /// Symbols and files this process has deliberately forgotten since it loaded.
    ///
    /// Saving has to merge rather than overwrite, or a scan running beside
    /// another agent writes back its own view and drops that agent's work. But a
    /// plain union would also resurrect everything a re-scan just purged, so the
    /// removals are recorded and subtracted from the merge.
    forgotten_symbols: RwLock<HashSet<String>>,
    forgotten_files: RwLock<HashSet<String>>,
    /// The absolute directory the relative keys are resolved against, for the
    /// most recent scan and as the fallback for a file with no recorded root.
    ///
    /// Symbol keys and the file-path maps are stored relative to the scan root,
    /// so the index and the Merkle root over it are the same on any machine:
    /// `crates/axiom-ast/src/lib.rs::AstIndex`, not `C:/dev/.../lib.rs::AstIndex`.
    /// That is what lets the index be committed and a ledger's root be compared
    /// across machines. The filesystem still needs the absolute path, so this
    /// holds the root a relative key is joined onto: set from the scanned path
    /// on a scan, and re-derived from where the index lives on a load, so a
    /// repository that moved still resolves.
    scan_root: RwLock<Option<PathBuf>>,
    /// The absolute root each file was scanned under.
    ///
    /// One index can hold two disjoint subtrees, scanned separately: their keys
    /// are each relative to their own scan root, so a file resolves and is
    /// purged against the root it came from, not whichever scan ran last. On a
    /// load the whole index shares one root, the workspace, so this maps every
    /// file to it. Not persisted; rebuilt on load.
    file_roots: RwLock<HashMap<String, PathBuf>>,
    /// The file each symbol was indexed from, the inverse of `file_to_symbols`.
    ///
    /// `file_of_symbol` and `language_of_symbol` are asked on every eval, and
    /// answered them by scanning every file's symbol list; this maps a symbol to
    /// its file directly. Maintained at the one attribution point in
    /// `index_node_at` and cleared for a symbol in `forget_file`, so it cannot
    /// hold a stale file for a symbol that has moved or gone. Rebuilt from
    /// `file_to_symbols` on load; not persisted, since it is derivable.
    symbol_to_file: RwLock<HashMap<String, String>>,
    /// The lines each symbol was declared on, used to attribute a call site to
    /// the function it sits inside rather than to the whole file.
    ///
    /// A list rather than a line, because two declarations can share one key:
    /// a file-keyed language has nowhere to record the difference between the
    /// `#[cfg(windows)]` and `#[cfg(unix)]` spellings of one function. Keeping
    /// only the last of them moved the key's line to the bottom of the file, so
    /// every call inside the first was charged to whatever symbol preceded it.
    ///
    /// Scan-scoped and not persisted: it exists only long enough for
    /// `resolve_reference_edges` to turn references into dependencies, which
    /// are what survive to disk.
    symbol_lines: RwLock<HashMap<String, Vec<usize>>>,
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
            type_hierarchy: RwLock::new(HashMap::new()),
            interface_implementors: RwLock::new(HashMap::new()),
            forgotten_symbols: RwLock::new(HashSet::new()),
            forgotten_files: RwLock::new(HashSet::new()),
            scan_root: RwLock::new(None),
            file_roots: RwLock::new(HashMap::new()),
            symbol_to_file: RwLock::new(HashMap::new()),
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
        self.index_node_at(symbol, kind, content, "", deps, None)
    }

    /// Register an OOP inheritance or interface implementation relationship.
    pub fn register_inheritance(&self, child: &str, parent: &str) {
        if child.is_empty() || parent.is_empty() || child == parent {
            return;
        }
        let mut hier = self.type_hierarchy.write().unwrap();
        let parents = hier.entry(child.to_string()).or_default();
        if !parents.iter().any(|p| p == parent) {
            parents.push(parent.to_string());
        }
        drop(hier);

        let mut impls = self.interface_implementors.write().unwrap();
        let children = impls.entry(parent.to_string()).or_default();
        if !children.iter().any(|c| c == child) {
            children.push(child.to_string());
        }
    }

    /// Look up all implementing or derived subclasses for a given interface or base class.
    pub fn get_implementors(&self, type_name: &str) -> Vec<String> {
        let mut results = HashSet::new();
        let impls = self.interface_implementors.read().unwrap();
        let simple = Self::simple_name_of(type_name);

        for key in [type_name, simple] {
            if let Some(children) = impls.get(key) {
                for c in children {
                    results.insert(c.clone());
                }
            }
        }
        results.into_iter().collect()
    }

    /// Look up all superclasses or interfaces that a given type implements or extends.
    pub fn get_supertypes(&self, type_name: &str) -> Vec<String> {
        let mut results = HashSet::new();
        let hier = self.type_hierarchy.read().unwrap();
        let simple = Self::simple_name_of(type_name);

        for key in [type_name, simple] {
            if let Some(parents) = hier.get(key) {
                for p in parents {
                    results.insert(p.clone());
                }
            }
        }
        results.into_iter().collect()
    }

    /// Return all distinct file extensions present in the indexed workspace.
    pub fn detected_languages(&self) -> Vec<String> {
        let mut set = HashSet::new();
        let map = self.file_to_symbols.read().unwrap();
        for file in map.keys() {
            if let Some(ext) = Path::new(file).extension().and_then(|e| e.to_str()) {
                set.insert(ext.to_ascii_lowercase());
            }
        }
        set.into_iter().collect()
    }

    /// Insert a node, recording the lines its declaration spans.
    ///
    /// `declared_on` is a zero-based inclusive line range, which is one line
    /// for most declarations and several for a wrapped parameter list. It does
    /// two jobs. It is what lets a reference found later be charged to the
    /// function it sits in: without it the only available owner is the file,
    /// and in a language where one file holds forty unrelated tests, charging
    /// all of them for one reference is the same as charging none of them. And
    /// it is what `source_range` reports, one-based, so a caller can open the
    /// file and find the declaration where the node says it is.
    /// The lines a symbol's body occupies, starting at its declaration.
    ///
    /// `structure` is the comment- and string-stripped text, so a brace inside a
    /// format string does not open a block, and `strip_comments_and_strings`
    /// preserves the line count, which is what lets the two be indexed together.
    ///
    /// A declaration that opens a brace runs until the brace closes. One that
    /// does not runs while the indentation stays deeper, which is Python, and
    /// also `fun f() = expr` in Kotlin and `def f = expr` in Scala.
    fn body_span(structure: &[&str], start: usize) -> (usize, usize) {
        let Some(decl) = structure.get(start) else {
            return (start, start);
        };
        let indent = decl.len() - decl.trim_start().len();
        let opens = decl.matches('{').count();
        let closes = decl.matches('}').count();

        if opens > closes {
            let mut depth = opens - closes;
            for (offset, line) in structure.iter().enumerate().skip(start + 1) {
                depth += line.matches('{').count();
                depth = depth.saturating_sub(line.matches('}').count());
                if depth == 0 {
                    return (start + 1, offset + 1);
                }
            }
            return (start + 1, structure.len());
        }

        let mut end = start + 1;
        while end < structure.len() {
            let line = structure[end];
            if !line.trim().is_empty() {
                let this = line.len() - line.trim_start().len();
                if this <= indent {
                    break;
                }
            }
            end += 1;
        }
        (start + 1, end)
    }

    /// The raw text of a symbol's body, for hashing.
    fn body_of(raw: &[&str], structure: &[&str], start: usize) -> String {
        let (from, to) = Self::body_span(structure, start);
        if from >= to || from >= raw.len() {
            return String::new();
        }
        raw[from..to.min(raw.len())].join("\n")
    }

    pub fn index_node_at(
        &self,
        symbol: &str,
        kind: &str,
        content: &str,
        // The symbol's body, which is what the hash exists to cover. Empty for a
        // node inserted by hand and for a declaration with no body.
        body: &str,
        deps: Vec<String>,
        declared_on: Option<(usize, usize)>,
    ) -> AstNode {
        if let Some((line, _)) = declared_on {
            let mut lines = self.symbol_lines.write().unwrap();
            let seen = lines.entry(symbol.to_string()).or_default();
            if !seen.contains(&line) {
                seen.push(line);
            }
        }
        let normalized = content.trim();
        let mut hasher = blake3::Hasher::new();
        hasher.update(normalized.as_bytes());
        // The body, not only the declaration. Hashing the declaration alone
        // meant editing what a multi-line function does left its hash where it
        // was, so `closure_hash`, which is a digest over these, did not move
        // either: a verdict cache would have reported a pass for code that
        // changed, past every guard, because the closure still looked complete.
        //
        // The separator keeps a declaration and a body from running together
        // into the same bytes as a different split of the same text.
        hasher.update(b"\0body\0");
        hasher.update(body.trim().as_bytes());
        for dep in &deps {
            hasher.update(dep.as_bytes());
        }
        let hash = hasher.finalize().to_hex().to_string();

        let node = AstNode {
            id: format!("node_{}", &hash[..12]),
            symbol_path: symbol.to_string(),
            kind: kind.to_string(),
            hash: hash.clone(),
            // One-based and inclusive, so `sed -n 'start,endp'` on the file
            // this symbol was indexed from prints the declaration. `(0, 0)`
            // means the parser recorded no position, which is what a node
            // inserted by hand through `index_node` has.
            source_range: declared_on.map_or((0, 0), |(a, b)| (a + 1, b + 1)),
            docstring: None,
            // The declaration as it was read. It used to be the symbol path,
            // which the response already carries as `symbol_path`, so the one
            // thing a caller wanted was the one thing that was thrown away.
            signature: Some(normalized.to_string()),
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
        if let Some(file) = PARSING_FILE.with(|f| f.borrow().clone()).as_ref() {
            let mut owned = self.file_to_symbols.write().unwrap();
            let entry = owned.entry(file.clone()).or_default();
            if !entry.iter().any(|s| s == symbol) {
                entry.push(symbol.to_string());
            }
            self.symbol_to_file
                .write()
                .unwrap()
                .insert(symbol.to_string(), file.clone());
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
        // Direct, not a scan of every file's symbol list. `symbol_to_file` is
        // the maintained inverse of `file_to_symbols`.
        let rel = self
            .symbol_to_file
            .read()
            .unwrap()
            .get(&canonical)
            .cloned()?;
        // The stored path is relative to the scan root; callers open it, so the
        // absolute path is returned by joining the root back on. This is the
        // one accessor that resolves a key to a real file, so it is the one
        // place the join lives.
        Some(self.resolve_path(&rel))
    }

    /// Join a stored, root-relative file path back onto the scan root, giving an
    /// absolute path a caller can open. An already-absolute path, or a missing
    /// scan root, is returned unchanged.
    pub fn resolve_path(&self, file_path: &str) -> String {
        if Path::new(file_path).is_absolute() {
            return file_path.to_string();
        }
        // The file's own scan root first, then the last scan's root as a
        // fallback, then the key unchanged when nothing is known.
        let root = self
            .file_roots
            .read()
            .unwrap()
            .get(file_path)
            .cloned()
            .or_else(|| self.scan_root.read().unwrap().clone());
        match root {
            Some(root) => root.join(file_path).to_string_lossy().replace('\\', "/"),
            None => file_path.to_string(),
        }
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
        let file = self
            .symbol_to_file
            .read()
            .unwrap()
            .get(&canonical)
            .cloned()?;
        Path::new(&file)
            .extension()
            .map(|e| e.to_string_lossy().to_string())
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

    pub const SKIP_DIRS: &[&str] = &[
        "target",
        "node_modules",
        "build",
        "dist",
        "vendor",
        "venv",
        ".venv",
        "__pycache__",
        ".mypy_cache",
        ".pytest_cache",
        ".gradle",
    ];

    pub const SOURCE_EXTS: &[&str] = &[
        "java", "rs", "py", "js", "ts", "jsx", "tsx", "mjs", "cjs", "go", "kt", "scala", "c",
        "cpp", "cc", "cxx", "h", "hpp", "json", "toml",
    ];

    fn fingerprint_dir(dir: &Path, out: &mut Vec<String>) {
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.') || Self::SKIP_DIRS.contains(&name) {
                    continue;
                }
                Self::fingerprint_dir(&path, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if !Self::SOURCE_EXTS.contains(&ext) {
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
        let mut queue: VecDeque<(String, usize, Vec<String>)> = VecDeque::new();
        let mut impacted_tests: Vec<String> = Vec::new();
        let mut causal_paths: HashMap<String, Vec<String>> = HashMap::new();

        // Seed queue with canonical symbol, unqualified name, and class prefix
        queue.push_back((canonical_symbol.clone(), 0, vec![canonical_symbol.clone()]));
        visited.insert(canonical_symbol.clone());

        if simple_name != canonical_symbol {
            queue.push_back((simple_name.to_string(), 0, vec![canonical_symbol.clone()]));
            visited.insert(simple_name.to_string());
        }

        let class_symbol = canonical_symbol
            .split("::")
            .next()
            .unwrap_or(&canonical_symbol);
        if class_symbol != canonical_symbol && visited.insert(class_symbol.to_string()) {
            queue.push_back((class_symbol.to_string(), 0, vec![canonical_symbol.clone()]));
        }

        let mut tests_by_depth: HashMap<usize, Vec<String>> = HashMap::new();
        let mut tests_recorded: HashSet<String> = HashSet::new();
        let mut impacted_set: HashSet<String> = HashSet::new();

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

        while let Some((curr, depth, path)) = queue.pop_front() {
            if let Some(node) = nodes.get(&curr) {
                if node.kind == "test" {
                    let d = depth.max(1);
                    if tests_recorded.insert(curr.clone()) {
                        tests_by_depth.entry(d).or_default().push(curr.clone());
                        causal_paths
                            .entry(curr.clone())
                            .or_insert_with(|| path.clone());
                    }
                    if d <= max_depth && impacted_set.insert(curr.clone()) {
                        impacted_tests.push(curr.clone());
                        causal_paths
                            .entry(curr.clone())
                            .or_insert_with(|| path.clone());
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
                                let mut next_path = path.clone();
                                next_path.push(caller.clone());
                                queue.push_back((caller.clone(), depth + 1, next_path));
                            }
                        }
                    }

                    // OOP Interface & Class Hierarchy Propagation:
                    // If key is an interface or base class, traverse all derived subclasses/implementors
                    for impl_cls in self.get_implementors(key) {
                        if visited.insert(impl_cls.clone()) {
                            let mut next_path = path.clone();
                            next_path.push(impl_cls.clone());
                            queue.push_back((impl_cls, depth + 1, next_path));
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
                                    if node.kind == "test" && impacted_set.insert(sym.clone()) {
                                        impacted_tests.push(sym.clone());
                                        causal_paths.entry(sym.clone()).or_insert_with(|| {
                                            vec![canonical_symbol.clone(), sym.clone()]
                                        });
                                        if tests_recorded.insert(sym.clone()) {
                                            tests_by_depth.entry(1).or_default().push(sym.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Expand any impacted test classes to include their individual test methods
        let test_classes: Vec<String> = impacted_tests
            .iter()
            .filter(|s| !s.contains("::"))
            .cloned()
            .collect();

        if !test_classes.is_empty() {
            let mut method_expansions = Vec::new();
            for (sym, node) in nodes.iter() {
                if node.kind == "test" && sym.contains("::") {
                    let class_prefix = sym.split("::").next().unwrap_or("");
                    if test_classes.iter().any(|c| c == class_prefix)
                        && impacted_set.insert(sym.clone())
                    {
                        method_expansions.push(sym.clone());
                        let mut p = causal_paths.get(class_prefix).cloned().unwrap_or_else(|| {
                            vec![canonical_symbol.clone(), class_prefix.to_string()]
                        });
                        p.push(sym.clone());
                        causal_paths.entry(sym.clone()).or_insert(p);
                        if tests_recorded.insert(sym.clone()) {
                            tests_by_depth.entry(1).or_default().push(sym.clone());
                        }
                    }
                }
            }
            impacted_tests.extend(method_expansions);
        }

        // Fallback: a test whose own name carries the symbol's, for the case
        // where nothing in the graph reaches it.
        if impacted_tests.is_empty() && !simple_name.is_empty() {
            let test_pattern_1 = format!("{}Test", simple_name);
            let test_pattern_2 = format!("test{}", simple_name);
            let call_pattern_1 = format!("{}.", simple_name);
            let call_pattern_2 = format!("{}::", simple_name);

            for (sym, node) in nodes.iter() {
                if node.kind == "test"
                    && (sym.contains(&test_pattern_1)
                        || sym.contains(&test_pattern_2)
                        || sym.contains(&canonical_symbol)
                        || sym.contains(&call_pattern_1)
                        || sym.contains(&call_pattern_2)
                        || node
                            .dependencies
                            .iter()
                            .any(|d| d == simple_name || d == &canonical_symbol))
                    && impacted_set.insert(sym.clone())
                {
                    impacted_tests.push(sym.clone());
                    causal_paths
                        .entry(sym.clone())
                        .or_insert_with(|| vec![canonical_symbol.clone(), sym.clone()]);
                    tests_by_depth.entry(1).or_default().push(sym.clone());
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
            causal_paths,
            total_tests_in_repo: total_tests,
            pruned_test_percentage: pruned_percentage,
        })
    }

    /// Generate a token-budgeted adaptive context slice for a symbol.
    /// Includes the symbol declaration, signature, docstring, and immediate
    /// callers and callees, truncated if necessary to respect the budget.
    pub fn get_symbol_slice(
        &self,
        symbol_path: &str,
        token_budget: Option<usize>,
    ) -> Option<SymbolContextSlice> {
        let node = self.get_symbol(symbol_path)?;
        let budget = token_budget.unwrap_or(500);

        let rev = self.reverse_deps.read().unwrap();
        let nodes = self.nodes.read().unwrap();

        let mut callers = Vec::new();
        let simple = Self::simple_name_of(&node.symbol_path);
        let keys = [node.symbol_path.as_str(), simple];
        for k in keys
            .iter()
            .take(if simple == node.symbol_path { 1 } else { 2 })
        {
            if let Some(c_set) = rev.get(*k) {
                for c in c_set {
                    if !callers.contains(c) {
                        callers.push(c.clone());
                    }
                }
            }
        }
        callers.sort();

        let callees = node.dependencies.clone();

        // Format the adaptive context slice
        let mut rendered = String::new();
        rendered.push_str(&format!(
            "// Symbol: {} [{}]\n",
            node.symbol_path, node.kind
        ));
        if let Some(doc) = &node.docstring {
            for line in doc.lines() {
                rendered.push_str(&format!("/// {}\n", line));
            }
        }
        if let Some(sig) = &node.signature {
            rendered.push_str(&format!("{};\n", sig));
        }

        let mut truncated = false;
        if !callers.is_empty() {
            rendered.push_str(&format!("\n// Immediate Callers ({}):\n", callers.len()));
            for caller in &callers {
                if (rendered.len() / 4) >= budget {
                    rendered.push_str("// ... [callers truncated for token budget]\n");
                    truncated = true;
                    break;
                }
                if let Some(c_node) = nodes.get(caller) {
                    if let Some(c_sig) = &c_node.signature {
                        rendered.push_str(&format!("// - {}: {}\n", caller, c_sig));
                    } else {
                        rendered.push_str(&format!("// - {}\n", caller));
                    }
                } else {
                    rendered.push_str(&format!("// - {}\n", caller));
                }
            }
        }

        if !callees.is_empty() {
            rendered.push_str(&format!(
                "\n// Dependencies / Callees ({}):\n",
                callees.len()
            ));
            for callee in &callees {
                if (rendered.len() / 4) >= budget {
                    rendered.push_str("// ... [dependencies truncated for token budget]\n");
                    truncated = true;
                    break;
                }
                if let Some(c_node) = nodes.get(callee) {
                    if let Some(c_sig) = &c_node.signature {
                        rendered.push_str(&format!("// - {}: {}\n", callee, c_sig));
                    } else {
                        rendered.push_str(&format!("// - {}\n", callee));
                    }
                } else {
                    rendered.push_str(&format!("// - {}\n", callee));
                }
            }
        }

        let estimated_tokens = (rendered.len() / 4).max(1);

        Some(SymbolContextSlice {
            symbol: node.symbol_path,
            kind: node.kind,
            signature: node.signature,
            docstring: node.docstring,
            estimated_tokens,
            callers,
            callees,
            dependencies: node.dependencies,
            truncated,
            rendered_slice: rendered,
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
        let mut visited: HashSet<String> = HashSet::new();

        // The absolute root the relative keys are resolved against. Symbol keys
        // are stored relative to it, so joining it back on is the only place the
        // filesystem sees an absolute path.
        let abs_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        *self.scan_root.write().unwrap() = Some(abs_root.clone());

        // Collect the files first, sequentially: reading the tree, dropping what
        // each file contributed last time, and adding it to the trigram store.
        // These touch shared state and are cheap; the parse that follows is the
        // expensive part and is done in parallel.
        let mut files: Vec<(String, String, String)> = Vec::new();
        self.walk_dir(root, root, &mut files, &mut visited)?;
        let files_scanned = files.len();

        // Record which root each file was scanned under, so a later scan of a
        // different subtree resolves and purges this one against its own root.
        {
            let mut roots = self.file_roots.write().unwrap();
            for (rel, _, _) in &files {
                roots.insert(rel.clone(), abs_root.clone());
            }
        }

        // Parse in parallel. Every symbol carries its file in its key, and the
        // current-file attribution is a thread-local, so two files parsed at
        // once do not cross. Each file counts its own nodes; the counts are
        // summed. Everything a parser writes goes through an RwLock, and the
        // reference resolution that needs the whole tree runs afterwards.
        let nodes_extracted: usize = files
            .par_iter()
            .map(|(rel, ext, content)| {
                let mut n = 0;
                self.parse_file_content(rel, ext, content, &mut n);
                n
            })
            .sum();

        // A scan is a statement about what the tree contains now, so anything
        // recorded from a file that has since disappeared has to go. Without
        // this the index only ever grows: a deleted class stays answerable and
        // a renamed method keeps its old name alongside the new one, and the
        // blast radius then names tests that no longer exist.
        self.forget_missing_files(&abs_root, &visited);
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

    /// A file's key: its path below the walk root, with forward slashes and no
    /// absolute prefix, so the key is the same on any machine.
    ///
    /// It used to be the absolute root followed by the relative path, which
    /// baked one machine's filesystem into every symbol and into the Merkle
    /// root over them. The absolute root is kept once, in `scan_root`, and
    /// joined back on only where the filesystem is actually touched.
    fn key_under_root(root: &Path, path: &Path) -> String {
        match path.strip_prefix(root) {
            Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
            Err(_) => {
                let root_str = root.to_string_lossy().replace('\\', "/");
                let path_str = path.to_string_lossy().replace('\\', "/");
                let root_norm = root_str.trim_start_matches("//?/").trim_end_matches('/');
                let path_norm = path_str.trim_start_matches("//?/");
                if let Some(rest) = path_norm.strip_prefix(root_norm) {
                    rest.trim_start_matches('/').to_string()
                } else {
                    Self::canonical_key(path)
                }
            }
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
        self.file_roots.write().unwrap().remove(file_path);
        self.forgotten_files
            .write()
            .unwrap()
            .insert(file_path.to_string());

        if let Some(symbols) = previous {
            let mut nodes = self.nodes.write().unwrap();
            let mut forgotten = self.forgotten_symbols.write().unwrap();
            let mut lines = self.symbol_lines.write().unwrap();
            let mut sym_file = self.symbol_to_file.write().unwrap();
            let mut hier = self.type_hierarchy.write().unwrap();
            let mut impls = self.interface_implementors.write().unwrap();

            for symbol in symbols {
                // Only drop the symbol itself if this file is still its owner.
                // Two files can declare one key, a package-keyed Java class of
                // the same name among them, and a stale file being forgotten
                // must not delete a symbol another file has since re-declared.
                // `symbol_to_file` records the current owner.
                let owned_by_this = sym_file
                    .get(&symbol)
                    .map(|f| f == file_path)
                    .unwrap_or(true);
                if owned_by_this {
                    nodes.remove(&symbol);
                    lines.remove(&symbol);
                    sym_file.remove(&symbol);
                    forgotten.insert(symbol.clone());

                    let simple = Self::simple_name_of(&symbol).to_string();
                    if let Some(parents) = hier.remove(&symbol) {
                        for p in parents {
                            if let Some(children) = impls.get_mut(&p) {
                                children.retain(|c| c != &symbol && c != &simple);
                            }
                        }
                    }
                    if let Some(parents) = hier.remove(&simple) {
                        for p in parents {
                            if let Some(children) = impls.get_mut(&p) {
                                children.retain(|c| c != &symbol && c != &simple);
                            }
                        }
                    }
                    impls.remove(&symbol);
                    impls.remove(&simple);
                }
            }
        }
    }

    /// Forget files recorded under this root by an earlier scan that this one did
    /// not see and that are no longer on disk, or obsolete non-portable keys.
    ///
    /// Scoped to the root on purpose. A scan is a statement about the tree it was
    /// pointed at and says nothing about anything else, so records from other
    /// roots are left alone whether or not their files still exist. Widening this
    /// to every recorded path makes one scan able to empty an unrelated project's
    /// entries out of a shared index.
    fn forget_missing_files(&self, abs_root: &Path, visited: &HashSet<String>) {
        let recorded: Vec<String> = self
            .file_to_symbols
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        let file_roots = self.file_roots.read().unwrap().clone();
        for file_path in recorded {
            if visited.contains(&file_path) {
                continue;
            }
            // If the key is an absolute path, it is a non-portable key
            // that must be purged when rescanning.
            if Path::new(&file_path).is_absolute() || file_path.contains(':') {
                self.forget_file(&file_path);
                continue;
            }
            // Resolve against the root this file was scanned under, not the
            // current scan's: a scan of one subtree must not judge a file from a
            // different subtree missing just because it is not under the tree
            // being scanned now.
            let root = file_roots
                .get(&file_path)
                .map(|p| p.as_path())
                .unwrap_or(abs_root);
            let on_disk = root.join(&file_path);
            if root == abs_root || !on_disk.exists() {
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
                .filter_map(|s| lines.get(s).map(|ls| (s, ls)))
                .flat_map(|(s, ls)| ls.iter().map(move |l| (*l, s)))
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

    /// Walk the tree, collecting each indexable file as `(key, ext, content)`.
    ///
    /// The read, the purge of what a file held last time, and the trigram index
    /// happen here, sequentially, because they touch shared state and are cheap.
    /// The caller parses the collected files in parallel, which is the part
    /// worth spreading across cores.
    fn walk_dir(
        &self,
        dir: &Path,
        root: &Path,
        files: &mut Vec<(String, String, String)>,
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
                // Directories whose contents are dependencies or build output,
                // not the codebase under study. Indexing them buries the real
                // symbols and fills the trigram store with vendored source.
                // Hidden directories are skipped too, `.git` among them.
                if !dir_name.starts_with('.') && !Self::SKIP_DIRS.contains(&dir_name) {
                    self.walk_dir(&path, root, files, visited)?;
                }
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if Self::SOURCE_EXTS.contains(&ext) {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            let rel = Self::key_under_root(root, &path);
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

                            files.push((rel, ext.to_string(), content));
                        }
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
        // Set the calling thread's current file, so attribution in
        // `index_node_at` is correct even when other threads are parsing other
        // files at the same time. Cleared at the end whatever happens.
        PARSING_FILE.with(|f| *f.borrow_mut() = Some(file_path.to_string()));
        self.parse_by_language(file_path, ext, content, nodes_count);
        PARSING_FILE.with(|f| *f.borrow_mut() = None);
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

        let clean = Self::strip_comments_and_strings(
            content,
            matches!(ext, "py" | "js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs"),
        );
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
            "ts" | "js" | "tsx" | "jsx" | "mjs" | "cjs" => {
                self.parse_ts_js_content(file_path, content, nodes_count)
            }
            "go" => self.parse_go_content(file_path, content, nodes_count),
            "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => {
                self.parse_c_cpp_content(file_path, content, nodes_count)
            }
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

    fn strip_comments_and_strings(content: &str, single_quotes_are_strings: bool) -> String {
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
                        // A line continuation escapes the newline, and a
                        // newline dropped here moves every line below it.
                        if chars[i + 1] == '\n' {
                            result.push('\n');
                        }
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
            } else if let Some(len) = Self::quoted_run_len(&chars, i, single_quotes_are_strings) {
                // Only a quote that closes is skipped past; a lifetime is
                // left to the branch below. See `quoted_run_len`.
                result.push(' ');
                i += len;
                result.push(' ');
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        result
    }

    /// How many characters the apostrophe at `i` consumes, when it opens
    /// something that closes.
    ///
    /// Rust and the JVM languages write a character as `'a'` or `'\n'`, and a
    /// Rust lifetime opens with the same character and never closes. Scanning
    /// to the next apostrophe in the file therefore ran from `Holder<'a>` to
    /// wherever the next one happened to be, dropping the newlines in between:
    /// every line number after the first lifetime in a file was short by one,
    /// and `record_references` charged each call to the function declared above
    /// the one it sits in. Returning `None` for a lifetime leaves the
    /// apostrophe as ordinary punctuation, which is what it is.
    ///
    /// Python, JavaScript and TypeScript spell a string this way instead. Those
    /// hold anything, but they end on the line they start, so an unclosed one
    /// is a syntax error rather than something to skip past.
    fn quoted_run_len(chars: &[char], i: usize, single_quotes_are_strings: bool) -> Option<usize> {
        if chars.get(i) != Some(&'\'') {
            return None;
        }

        if single_quotes_are_strings {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '\n' {
                match chars[j] {
                    '\\' => j += 2,
                    '\'' => return Some(j - i + 1),
                    _ => j += 1,
                }
            }
            return None;
        }

        if chars.get(i + 1) == Some(&'\\') {
            // An escape, up to the longest form there is: '\u{10FFFF}'.
            let limit = (i + 12).min(chars.len());
            for (j, c) in chars.iter().enumerate().take(limit).skip(i + 2) {
                match c {
                    '\'' => return Some(j - i + 1),
                    '\n' => return None,
                    _ => {}
                }
            }
            return None;
        }

        match (chars.get(i + 1), chars.get(i + 2)) {
            (Some(c), Some('\'')) if *c != '\'' && *c != '\n' => Some(3),
            _ => None,
        }
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
        let clean_code = Self::strip_comments_and_strings(content, false);

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
        let structure_text = Self::strip_comments_and_strings(content, true);
        let structure: Vec<&str> = structure_text.lines().collect();
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
                    || trimmed.starts_with("sealed ")
                    || trimmed.starts_with("non-sealed ")
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
                            .split('(')
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

                            // Extract OOP inheritance & interface implementations
                            let mut in_ext_or_impl = false;
                            for &token in &tokens[pos + 2..] {
                                let clean = token.trim_matches(|c: char| {
                                    c == '{' || c == ',' || c == ';' || c == '(' || c == ')'
                                });
                                let base = clean.split('<').next().unwrap_or(clean).trim();
                                let base_ident = base.split('.').next_back().unwrap_or(base).trim();
                                if token == "extends"
                                    || token == "implements"
                                    || token == "with"
                                    || token == ":"
                                {
                                    in_ext_or_impl = true;
                                } else if in_ext_or_impl && Self::is_valid_identifier(base_ident) {
                                    self.register_inheritance(&full_symbol, base);
                                    self.register_inheritance(&full_symbol, base_ident);
                                    self.register_inheritance(class_name, base);
                                    self.register_inheritance(class_name, base_ident);
                                    if !node_deps.contains(&base.to_string()) {
                                        node_deps.push(base.to_string());
                                    }
                                    if base_ident != base
                                        && !node_deps.contains(&base_ident.to_string())
                                    {
                                        node_deps.push(base_ident.to_string());
                                    }
                                }
                            }

                            self.index_node_at(
                                &full_symbol,
                                kind,
                                trimmed,
                                &Self::body_of(&lines, &structure, i),
                                node_deps,
                                Some((i, i)),
                            );
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
                || trimmed.starts_with("@RepeatedTest")
                || trimmed.starts_with("@TestFactory")
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
                // Where the declaration starts, before the join below walks
                // `i` to the end of a wrapped parameter list. Reporting the
                // last line as the position would point a caller at the line
                // the parameters happen to close on.
                let decl_start = i;
                let mut full_sig = trimmed.to_string();
                let mut is_annotated_test = full_sig.contains("@Test")
                    || full_sig.contains("@ParameterizedTest")
                    || full_sig.contains("@RepeatedTest")
                    || full_sig.contains("@TestFactory");

                if !is_annotated_test {
                    let mut prev_idx = decl_start;
                    while prev_idx > 0 {
                        prev_idx -= 1;
                        let prev_line = lines[prev_idx].trim();
                        if prev_line.starts_with('@') {
                            if prev_line.starts_with("@Test")
                                || prev_line.starts_with("@ParameterizedTest")
                                || prev_line.starts_with("@RepeatedTest")
                                || prev_line.starts_with("@TestFactory")
                            {
                                is_annotated_test = true;
                                break;
                            }
                        } else if !prev_line.is_empty() {
                            break;
                        }
                    }
                }

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
                            .split(',')
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

                    self.index_node_at(
                        &full_symbol,
                        kind,
                        signature_clean,
                        // From the last line of the signature: a wrapped
                        // parameter list means the body starts after `i`, not
                        // after the line the declaration opened on.
                        &Self::body_of(&lines, &structure, i),
                        node_deps,
                        Some((decl_start, i)),
                    );
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

    /// A Rust symbol carries the type it is declared under.
    ///
    /// Keyed by file and short name alone, two impls in one file collide. This
    /// repository declares `search` on both `AstIndex` and `ZoektIndex` in
    /// `lib.rs`: the second overwrote the first, leaving one node holding the
    /// second's dependencies and hash under a name an agent reads as the first.
    /// The blast radius then missed a test that really fails, because
    /// `looks_like_a_pattern` is called from `AstIndex::search` and the edge
    /// went to a node that no longer existed.
    ///
    /// Modules are not tracked, so two same-named functions in two `mod` blocks
    /// of one file still share a key. `symbol_lines` keeps every declaration
    /// line for exactly that case, so the calls inside each are still charged
    /// to the right key even when the key cannot tell them apart.
    fn parse_rust_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut uses = Vec::new();

        // Braces are counted on comment- and string-stripped text: a brace in a
        // format string would otherwise open a block that never closes, and
        // every function below it would be filed under the wrong type.
        let clean = Self::strip_comments_and_strings(content, false);
        let counted: Vec<&str> = clean.lines().collect();
        let raw: Vec<&str> = content.lines().collect();
        let mut owner_stack: Vec<(String, usize)> = Vec::new();
        let mut depth: usize = 0;

        for (line_no, line) in content.lines().enumerate() {
            let braces = counted.get(line_no).copied().unwrap_or("");
            let trimmed = line.trim();
            // What counts as a declaration is read from the stripped text, not
            // from the raw line. A repository whose subject is parsing writes
            // source inside string literals constantly, and matching the raw
            // line indexed those fixtures: `blast_radius.rs::looks_like_a_pattern`
            // existed as a symbol because a test writes a Rust fixture as a
            // string. That is not only a larger index. It made the real
            // `looks_like_a_pattern` ambiguous, so `axiom symbol` refused a
            // real function by its real name.
            //
            // The raw line is still what gets stored, so a signature keeps the
            // string a declaration genuinely contains.
            let decl = braces.trim();

            let opens = braces.matches('{').count();
            let closes = braces.matches('}').count();

            if !(trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("*/"))
            {
                if let Some(owner) = Self::rust_owner_opened_by(decl) {
                    if decl.contains("trait ") {
                        let symbol = Self::rust_symbol_in(file_path, &owner_stack, &owner);
                        self.index_node_at(
                            &symbol,
                            "trait",
                            trimmed,
                            &Self::body_of(&raw, &counted, line_no),
                            uses.clone(),
                            Some((line_no, line_no)),
                        );
                        *nodes_count += 1;
                    } else if (decl.starts_with("impl ")
                        || decl.starts_with("impl<")
                        || decl.contains(" impl ")
                        || decl.contains(" impl<"))
                        && decl.contains(" for ")
                    {
                        let after_impl = if let Some(pos) = decl.find("impl") {
                            decl[pos + 4..].trim_start()
                        } else {
                            ""
                        };
                        let after_gen = Self::skip_angle_group(after_impl);
                        let parts: Vec<&str> = after_gen.split(" for ").collect();
                        if parts.len() == 2 {
                            let tr = parts[0].split('<').next().unwrap_or(parts[0]).trim();
                            let tr_name = tr.rsplit("::").next().unwrap_or(tr).trim();
                            let tokens: Vec<&str> = parts[1].split_whitespace().collect();
                            let mut st_name = "";
                            let mut raw_st = "";
                            for tok in tokens {
                                let clean = tok
                                    .trim_matches(|c: char| {
                                        c == '&'
                                            || c == '*'
                                            || c == '{'
                                            || c == '('
                                            || c == ')'
                                            || c == ';'
                                    })
                                    .split('<')
                                    .next()
                                    .unwrap_or(tok)
                                    .trim();
                                if clean.starts_with('\'')
                                    || clean == "mut"
                                    || clean == "const"
                                    || clean.is_empty()
                                {
                                    continue;
                                }
                                let ident = clean.rsplit("::").next().unwrap_or(clean).trim();
                                if Self::is_valid_identifier(ident) {
                                    st_name = ident;
                                    raw_st = clean;
                                    break;
                                }
                            }
                            if Self::is_valid_identifier(tr_name)
                                && Self::is_valid_identifier(st_name)
                            {
                                self.register_inheritance(st_name, tr_name);
                                if tr != tr_name || raw_st != st_name {
                                    self.register_inheritance(raw_st, tr);
                                }
                            }
                        }
                    }
                    owner_stack.push((owner, depth + opens.max(1)));
                } else if decl.starts_with("use ") {
                    uses.push(decl.replace("use ", "").replace(';', "").trim().to_string());
                } else if decl.contains("fn ") {
                    let before_paren = decl
                        .split('(')
                        .next()
                        .unwrap_or("")
                        .split('{')
                        .next()
                        .unwrap_or("");
                    let before_gen = before_paren.split('<').next().unwrap_or("");
                    let words: Vec<&str> = before_gen.split_whitespace().collect();
                    if words.len() >= 2 && words[words.len() - 2] == "fn" {
                        let name = words[words.len() - 1].trim();
                        if Self::is_valid_identifier(name) {
                            let symbol = Self::rust_symbol_in(file_path, &owner_stack, name);
                            let is_test = name.starts_with("test_") || decl.contains("#[test]");
                            let kind = if is_test { "test" } else { "function" };

                            self.index_node_at(
                                &symbol,
                                kind,
                                trimmed,
                                &Self::body_of(&raw, &counted, line_no),
                                uses.clone(),
                                Some((line_no, line_no)),
                            );
                            *nodes_count += 1;
                        }
                    }
                } else if decl.contains("struct ")
                    || decl.contains("enum ")
                    || decl.contains("trait ")
                {
                    let before_body = decl
                        .split('{')
                        .next()
                        .unwrap_or("")
                        .split(';')
                        .next()
                        .unwrap_or("");
                    let before_gen = before_body.split('<').next().unwrap_or("");
                    let words: Vec<&str> = before_gen.split_whitespace().collect();
                    if words.len() >= 2 {
                        let prev = words[words.len() - 2];
                        if prev == "struct" || prev == "enum" || prev == "trait" {
                            let name = words[words.len() - 1].trim();
                            if Self::is_valid_identifier(name) {
                                let kind = if prev == "trait" {
                                    "trait"
                                } else if prev == "enum" {
                                    "enum"
                                } else {
                                    "struct"
                                };
                                let symbol = Self::rust_symbol_in(file_path, &owner_stack, name);
                                self.index_node_at(
                                    &symbol,
                                    kind,
                                    trimmed,
                                    &Self::body_of(&raw, &counted, line_no),
                                    uses.clone(),
                                    Some((line_no, line_no)),
                                );
                                *nodes_count += 1;
                            }
                        }
                    }
                }
            }

            depth += opens;
            depth = depth.saturating_sub(closes);
            while let Some((_, closes_at)) = owner_stack.last() {
                if depth < *closes_at {
                    owner_stack.pop();
                } else {
                    break;
                }
            }
        }
    }

    /// The type whose methods a line opens, when it opens one.
    ///
    /// `impl Foo`, `impl<T> Foo<T>`, `impl Trait for Foo` and `trait Foo` all
    /// name the owner of the methods below them. The generic list is skipped
    /// with a depth counter rather than split on the closing angle, because
    /// `impl<T: Into<String>> Foo` closes two of them before the type starts.
    /// A Rust symbol key, scoped by every block it sits inside.
    ///
    /// Joining the whole stack rather than taking its top is what makes two
    /// modules declaring the same function two symbols. Before this, both were
    /// `file.rs::helper`, the second `index_node_at` overwrote the first, and
    /// the surviving node carried the second declaration's hash under a name
    /// that reads as either. A verdict cache keys on that hash, so a change to
    /// the first would not have moved it: a pass reported for code that changed,
    /// which is exactly what `closure_hash` returning `Option` exists to
    /// prevent, arriving by a route where the closure still looks complete.
    ///
    /// `#[cfg]`-guarded twins remain one key on purpose. `#[cfg(windows)] fn
    /// worth_retrying` and its `#[cfg(unix)] `sibling are one name in one scope,
    /// and only one of them is ever compiled, so a single node with both
    /// declaration lines recorded is the honest answer rather than a gap.
    fn rust_symbol_in(file_path: &str, owners: &[(String, usize)], name: &str) -> String {
        if owners.is_empty() {
            return format!("{file_path}::{name}");
        }
        let scope: Vec<&str> = owners.iter().map(|(o, _)| o.as_str()).collect();
        format!("{}::{}::{}", file_path, scope.join("::"), name)
    }

    fn rust_owner_opened_by(trimmed: &str) -> Option<String> {
        // `mod foo;` declares a module in another file and opens no scope here,
        // so it must not become an owner: doing so would file every symbol
        // below it under a module whose body is somewhere else.
        if trimmed.ends_with(';') {
            return None;
        }

        let after_keyword = [
            "impl",
            "trait",
            "pub trait",
            "pub(crate) trait",
            "mod",
            "pub mod",
            "pub(crate) mod",
        ]
        .iter()
        // `implements_something()` also starts with `impl`. A declaration
        // is followed by a space or by its generic list, never by more
        // identifier.
        .filter_map(|kw| trimmed.strip_prefix(*kw))
        .find(|rest| rest.starts_with(' ') || rest.starts_with('<'))?;

        let rest = Self::skip_angle_group(after_keyword.trim_start()).trim_start();
        // `impl Trait for Type`: the methods belong to the type, which is what
        // a caller changing one of them would name.
        let target = match rest.rfind(" for ") {
            Some(at) => &rest[at + " for ".len()..],
            None => rest,
        };

        let tokens: Vec<&str> = target.split_whitespace().collect();
        for tok in tokens {
            let clean = tok
                .trim_matches(|c: char| {
                    c == '&' || c == '*' || c == '{' || c == '(' || c == ')' || c == ';'
                })
                .split('<')
                .next()
                .unwrap_or(tok)
                .trim();
            if clean.starts_with('\'') || clean == "mut" || clean == "const" || clean.is_empty() {
                continue;
            }
            let ident = clean.rsplit("::").next().unwrap_or(clean).trim();
            if Self::is_valid_identifier(ident) {
                return Some(ident.to_string());
            }
        }
        None
    }

    /// Everything after a leading generic list, with nesting counted.
    fn skip_angle_group(s: &str) -> &str {
        if !s.starts_with('<') {
            return s;
        }
        let mut depth = 0usize;
        for (i, c) in s.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        return &s[i + 1..];
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn parse_python_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();
        let mut current_class = String::new();
        let raw: Vec<&str> = content.lines().collect();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("\"\"\"")
                || trimmed.starts_with("'''")
            {
                continue;
            }

            let is_indented = line.starts_with(' ') || line.starts_with('\t');
            if !is_indented && !trimmed.starts_with("class ") {
                current_class.clear();
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
                    if trimmed.contains('(') && trimmed.contains(')') {
                        if let Some(bases_str) =
                            trimmed.split('(').nth(1).and_then(|s| s.split(')').next())
                        {
                            for base in bases_str.split(',') {
                                let base_clean = base.trim();
                                let base_raw =
                                    base_clean.split('[').next().unwrap_or(base_clean).trim();
                                let base_name =
                                    base_raw.split('.').next_back().unwrap_or(base_raw).trim();
                                if Self::is_valid_identifier(base_name) {
                                    self.register_inheritance(&symbol, base_name);
                                    self.register_inheritance(&symbol, base_raw);
                                    self.register_inheritance(&name, base_name);
                                    self.register_inheritance(&name, base_raw);
                                }
                            }
                        }
                    }
                    let kind = if name.contains("Test") {
                        "test"
                    } else {
                        "class"
                    };
                    self.index_node_at(
                        &symbol,
                        kind,
                        trimmed,
                        &Self::body_of(&raw, &raw, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
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

                if Self::is_valid_identifier(&name) {
                    let symbol = if is_indented && !current_class.is_empty() {
                        format!("{}::{}::{}", file_path, current_class, name)
                    } else {
                        format!("{}::{}", file_path, name)
                    };
                    let is_test = name.starts_with("test_");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(
                        &symbol,
                        kind,
                        trimmed,
                        &Self::body_of(&raw, &raw, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            }
        }
    }

    fn parse_ts_js_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let mut imports = Vec::new();

        // What counts as a declaration is read from the stripped text. Matching
        // the raw line indexed `function ghost()` written inside a string, which
        // is how a fixture in a test file became a symbol and made a real name
        // ambiguous. The other parsers already avoided this; TypeScript was the
        // one that did not. The raw line is still what gets stored, so a
        // signature keeps any string the declaration genuinely contains.
        let clean = Self::strip_comments_and_strings(content, true);
        let stripped: Vec<&str> = clean.lines().collect();
        let raw: Vec<&str> = content.lines().collect();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let decl = stripped.get(line_no).copied().unwrap_or("").trim();
            if decl.starts_with("import ") {
                imports.push(trimmed.to_string());
            } else if decl.contains("function ")
                || decl.starts_with("export function ")
                || decl.starts_with("export async function ")
                || decl.starts_with("async function ")
            {
                let name = decl
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .split("function ")
                    .last()
                    .unwrap_or("")
                    .split('<')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("test")
                        || file_path.contains("test")
                        || file_path.contains("spec");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(
                        &symbol,
                        kind,
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            } else if decl.starts_with("class ")
                || decl.starts_with("export class ")
                || decl.starts_with("export default class ")
            {
                let name = decl
                    .split("class ")
                    .last()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split('<')
                    .next()
                    .unwrap_or("")
                    .replace('{', "")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    let tokens: Vec<&str> = decl.split_whitespace().collect();
                    let mut in_ext_or_impl = false;
                    for &t in &tokens {
                        let clean = t.trim_matches(|c: char| {
                            c == '{' || c == ',' || c == ';' || c == '(' || c == ')'
                        });
                        let base = clean.split('<').next().unwrap_or(clean).trim();
                        let base_ident = base.split('.').next_back().unwrap_or(base).trim();
                        if t == "extends" || t == "implements" {
                            in_ext_or_impl = true;
                        } else if in_ext_or_impl && Self::is_valid_identifier(base_ident) {
                            self.register_inheritance(&symbol, base);
                            self.register_inheritance(&symbol, base_ident);
                            self.register_inheritance(&name, base);
                            self.register_inheritance(&name, base_ident);
                        }
                    }

                    self.index_node_at(
                        &symbol,
                        "class",
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            } else if decl.starts_with("interface ")
                || decl.starts_with("export interface ")
                || decl.starts_with("export default interface ")
            {
                let name = decl
                    .split("interface ")
                    .last()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split('<')
                    .next()
                    .unwrap_or("")
                    .replace('{', "")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    let tokens: Vec<&str> = decl.split_whitespace().collect();
                    let mut in_ext = false;
                    for &t in &tokens {
                        let clean = t.trim_matches(|c: char| {
                            c == '{' || c == ',' || c == ';' || c == '(' || c == ')'
                        });
                        let base = clean.split('<').next().unwrap_or(clean).trim();
                        let base_ident = base.split('.').next_back().unwrap_or(base).trim();
                        if t == "extends" {
                            in_ext = true;
                        } else if in_ext && Self::is_valid_identifier(base_ident) {
                            self.register_inheritance(&symbol, base);
                            self.register_inheritance(&symbol, base_ident);
                            self.register_inheritance(&name, base);
                            self.register_inheritance(&name, base_ident);
                        }
                    }

                    self.index_node_at(
                        &symbol,
                        "interface",
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            } else if decl.starts_with("type ") || decl.starts_with("export type ") {
                let name = decl
                    .split("type ")
                    .last()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split('<')
                    .next()
                    .unwrap_or("")
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    self.index_node_at(
                        &symbol,
                        "type",
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            } else if decl.starts_with("enum ")
                || decl.starts_with("export enum ")
                || decl.starts_with("const enum ")
                || decl.starts_with("export const enum ")
            {
                let name = decl
                    .split("enum ")
                    .last()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .replace('{', "")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    self.index_node_at(
                        &symbol,
                        "enum",
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            } else if (decl.starts_with("const ")
                || decl.starts_with("export const ")
                || decl.starts_with("let ")
                || decl.starts_with("export let "))
                && (decl.contains("=>")
                    || decl.contains("= function")
                    || decl.contains("= async function"))
            {
                let after_kw = if decl.contains("const ") {
                    decl.split("const ").last().unwrap_or("")
                } else {
                    decl.split("let ").last().unwrap_or("")
                };
                let name = after_kw
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if !name.is_empty() && Self::is_valid_identifier(&name) {
                    let symbol = format!("{}::{}", file_path, name);
                    let is_test = name.starts_with("test")
                        || file_path.contains("test")
                        || file_path.contains("spec");
                    let kind = if is_test { "test" } else { "function" };

                    self.index_node_at(
                        &symbol,
                        kind,
                        trimmed,
                        &Self::body_of(&raw, &stripped, line_no),
                        imports.clone(),
                        Some((line_no, line_no)),
                    );
                    *nodes_count += 1;
                }
            }
        }
    }

    /// The receiver type and the method name on a Go `func` line.
    ///
    /// `func (a *Alpha) Search(q string) bool` is a method on `Alpha`. Taking
    /// everything before the first `(` as the name gives the empty string here,
    /// which is why methods were skipped entirely and a Go codebase held
    /// package-level free functions and nothing else.
    fn go_receiver_and_name(after_func: &str) -> (Option<String>, String) {
        let rest = after_func.trim_start();
        let Some(inner) = rest.strip_prefix('(') else {
            let name = rest
                .split('(')
                .next()
                .unwrap_or("")
                .split('[')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            return (None, name);
        };

        let Some(end) = inner.find(')') else {
            return (None, String::new());
        };
        // `a *Alpha` or `Alpha` or `s *Stack[T]`: the type is the last word, and a pointer
        // receiver is the same type as a value one.
        let raw_receiver = inner[..end]
            .split_whitespace()
            .next_back()
            .unwrap_or("")
            .trim_start_matches('*');
        let receiver = raw_receiver
            .split('[')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let name = inner[end + 1..]
            .split('(')
            .next()
            .unwrap_or("")
            .split('[')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        let owner = Self::is_valid_identifier(&receiver).then_some(receiver);
        (owner, name)
    }

    fn parse_go_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        // Declarations are read from the stripped text for the same reason as
        // every other parser: source written inside a string is not a
        // declaration.
        let clean = Self::strip_comments_and_strings(content, false);
        let stripped: Vec<&str> = clean.lines().collect();
        let raw: Vec<&str> = content.lines().collect();

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let decl = stripped.get(line_no).copied().unwrap_or("").trim();

            if let Some(after_func) = decl.strip_prefix("func ") {
                let (owner, name) = Self::go_receiver_and_name(after_func);
                if !Self::is_valid_identifier(&name) {
                    continue;
                }
                let symbol = match &owner {
                    Some(recv) => format!("{file_path}::{recv}::{name}"),
                    None => format!("{file_path}::{name}"),
                };
                // Go's convention, and what `go test` runs.
                let kind = if name.starts_with("Test") {
                    "test"
                } else {
                    "function"
                };
                self.index_node_at(
                    &symbol,
                    kind,
                    trimmed,
                    &Self::body_of(&raw, &stripped, line_no),
                    vec![],
                    Some((line_no, line_no)),
                );
                *nodes_count += 1;
            } else if let Some(after_type) = decl.strip_prefix("type ") {
                // `type Alpha struct {` and `type Reader interface {`. A type
                // alias, `type Meters float64`, is a declaration too and is
                // indexed as one rather than being dropped for lacking a
                // keyword.
                let name = after_type
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split('[')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('{')
                    .trim();
                if !Self::is_valid_identifier(name) {
                    continue;
                }
                let kind = if after_type.contains("interface") {
                    "interface"
                } else if after_type.contains("struct") {
                    "struct"
                } else {
                    "type"
                };
                let symbol = format!("{file_path}::{name}");
                self.index_node_at(
                    &symbol,
                    kind,
                    trimmed,
                    &Self::body_of(&raw, &stripped, line_no),
                    vec![],
                    Some((line_no, line_no)),
                );
                *nodes_count += 1;
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

    fn parse_c_cpp_content(&self, file_path: &str, content: &str, nodes_count: &mut usize) {
        let clean = Self::strip_comments_and_strings(content, false);
        let stripped: Vec<&str> = clean.lines().collect();
        let raw: Vec<&str> = content.lines().collect();

        let mut includes = Vec::new();
        let mut scope_stack: Vec<(String, usize)> = Vec::new();
        let mut depth = 0usize;

        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let decl = stripped.get(line_no).copied().unwrap_or("").trim();
            let opens = decl.matches('{').count();
            let closes = decl.matches('}').count();

            if decl.starts_with("#include ") {
                includes.push(trimmed.to_string());
            } else if !decl.starts_with('#')
                && !decl.starts_with("using ")
                && !decl.starts_with("typedef ")
            {
                if decl.starts_with("namespace ") {
                    let name = decl
                        .split("namespace ")
                        .last()
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .replace('{', "")
                        .trim()
                        .to_string();
                    if !name.is_empty() && name.split("::").all(Self::is_valid_identifier) {
                        scope_stack.push((name, depth + opens.max(1)));
                    }
                } else if (!decl.contains('(')
                    || decl.starts_with("class ")
                    || decl.starts_with("struct ")
                    || decl.starts_with("enum ")
                    || decl.starts_with("template"))
                    && (decl.contains("class ")
                        || decl.contains("struct ")
                        || decl.contains("enum "))
                    && !decl.contains("return ")
                    && (!decl.contains('(')
                        || decl.contains('{')
                            && decl.find('{').unwrap_or(0) < decl.find('(').unwrap_or(usize::MAX))
                {
                    let kind = if decl.contains("class ") {
                        "class"
                    } else if decl.contains("enum ") {
                        "enum"
                    } else {
                        "struct"
                    };

                    let after_kw = if decl.contains("class ") {
                        decl.split("class ").last().unwrap_or("")
                    } else if decl.contains("enum ") {
                        decl.split("enum ").last().unwrap_or("")
                    } else {
                        decl.split("struct ").last().unwrap_or("")
                    };

                    let name = after_kw
                        .split(':')
                        .next()
                        .unwrap_or("")
                        .split('{')
                        .next()
                        .unwrap_or("")
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .split('<')
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();

                    if !name.is_empty() && Self::is_valid_identifier(&name) {
                        let prefix = scope_stack
                            .iter()
                            .map(|(s, _)| s.as_str())
                            .collect::<Vec<_>>()
                            .join("::");
                        let symbol = if prefix.is_empty() {
                            format!("{}::{}", file_path, name)
                        } else {
                            format!("{}::{}::{}", file_path, prefix, name)
                        };

                        if after_kw.contains(':') {
                            if let Some(bases_part) = after_kw.split(':').nth(1) {
                                let bases_clean = bases_part
                                    .split('{')
                                    .next()
                                    .unwrap_or(bases_part)
                                    .split(';')
                                    .next()
                                    .unwrap_or(bases_part);
                                for base_entry in bases_clean.split(',') {
                                    let base_words: Vec<&str> =
                                        base_entry.split_whitespace().collect();
                                    for w in base_words {
                                        let clean_w = w.trim_matches(|c: char| {
                                            c == '{'
                                                || c == ';'
                                                || c == ','
                                                || c == '<'
                                                || c == '>'
                                                || c == '('
                                                || c == ')'
                                        });
                                        let base_ident = clean_w
                                            .split('<')
                                            .next()
                                            .unwrap_or(clean_w)
                                            .split('.')
                                            .next_back()
                                            .unwrap_or(clean_w)
                                            .trim();
                                        if clean_w != "public"
                                            && clean_w != "protected"
                                            && clean_w != "private"
                                            && clean_w != "virtual"
                                            && clean_w != "internal"
                                            && Self::is_valid_identifier(base_ident)
                                        {
                                            self.register_inheritance(&symbol, clean_w);
                                            self.register_inheritance(&symbol, base_ident);
                                            self.register_inheritance(&name, clean_w);
                                            self.register_inheritance(&name, base_ident);
                                        }
                                    }
                                }
                            }
                        }

                        self.index_node_at(
                            &symbol,
                            kind,
                            trimmed,
                            &Self::body_of(&raw, &stripped, line_no),
                            includes.clone(),
                            Some((line_no, line_no)),
                        );
                        *nodes_count += 1;

                        if opens > closes && (kind == "class" || kind == "struct") {
                            scope_stack.push((name, depth + opens.max(1)));
                        }
                    }
                } else if decl.contains('(')
                    && (opens > 0 || decl.ends_with(';') || decl.contains("->"))
                {
                    let before_paren = decl.split('(').next().unwrap_or("").trim();
                    let raw_token = before_paren
                        .split_whitespace()
                        .last()
                        .unwrap_or("")
                        .trim_start_matches('*')
                        .trim_start_matches('&');

                    let is_reserved = matches!(
                        raw_token,
                        "if" | "while" | "for" | "switch" | "catch" | "return" | "sizeof"
                    );

                    if !is_reserved && !raw_token.is_empty() {
                        let name = if before_paren.contains("operator") {
                            let op_part =
                                before_paren.split("operator").last().unwrap_or("").trim();
                            format!("operator{}", op_part)
                        } else {
                            raw_token.to_string()
                        };

                        let name_parts: Vec<&str> = name.split("::").collect();
                        let all_valid = name_parts
                            .iter()
                            .all(|p| p.starts_with("operator") || Self::is_valid_identifier(p));

                        if all_valid {
                            let prefix = scope_stack
                                .iter()
                                .map(|(s, _)| s.as_str())
                                .collect::<Vec<_>>()
                                .join("::");
                            let symbol = if prefix.is_empty() {
                                format!("{}::{}", file_path, name)
                            } else {
                                format!("{}::{}::{}", file_path, prefix, name)
                            };

                            let is_test = name.starts_with("test_")
                                || name.starts_with("Test")
                                || file_path.contains("test");
                            let kind = if is_test { "test" } else { "function" };

                            self.index_node_at(
                                &symbol,
                                kind,
                                trimmed,
                                &Self::body_of(&raw, &stripped, line_no),
                                includes.clone(),
                                Some((line_no, line_no)),
                            );
                            *nodes_count += 1;
                        }
                    }
                }
            }

            depth += opens;
            depth = depth.saturating_sub(closes);
            while let Some((_, closes_at)) = scope_stack.last() {
                if depth < *closes_at {
                    scope_stack.pop();
                } else {
                    break;
                }
            }
        }
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
                    type_hierarchy: HashMap::new(),
                    interface_implementors: HashMap::new(),
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
                type_hierarchy: HashMap::new(),
                interface_implementors: HashMap::new(),
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
            type_hierarchy: HashMap::new(),
            interface_implementors: HashMap::new(),
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
        payload
            .type_hierarchy
            .extend(self.type_hierarchy.read().unwrap().clone());
        payload
            .interface_implementors
            .extend(self.interface_implementors.read().unwrap().clone());

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

        // The keys are relative to the workspace, so a repository that moved
        // still resolves. The workspace is where the index lives: the parent of
        // its `.axiom` directory. `<workspace>/.axiom/index.json` gives
        // `<workspace>`; anything shallower falls back to the index's own
        // parent, which is correct when the index is not under a `.axiom`.
        let scan_root = abs_path
            .parent()
            .and_then(|axiom_dir| {
                if axiom_dir.file_name().and_then(|n| n.to_str()) == Some(".axiom") {
                    axiom_dir.parent()
                } else {
                    Some(axiom_dir)
                }
            })
            .map(|p| p.to_path_buf());

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
        // read, so re-read them here, joining the relative key back onto the
        // workspace root. Files that have since moved or been deleted are
        // skipped, which costs their text search rather than the whole load.
        let mut zoekt = ZoektIndex::new();
        for file_path in payload.file_to_symbols.keys() {
            let on_disk = match (&scan_root, Path::new(file_path).is_absolute()) {
                (Some(root), false) => root.join(file_path),
                _ => PathBuf::from(file_path),
            };
            if let Ok(text) = std::fs::read_to_string(&on_disk) {
                zoekt.add_document(file_path, &text);
            }
        }

        // The inverse of file_to_symbols, rebuilt on load rather than persisted.
        let mut symbol_to_file = HashMap::new();
        for (file, symbols) in &payload.file_to_symbols {
            for symbol in symbols {
                symbol_to_file.insert(symbol.clone(), file.clone());
            }
        }

        // A loaded index shares one root, the workspace, so every file resolves
        // against it. A subtree scanned separately later gets its own root then.
        let mut file_roots = HashMap::new();
        if let Some(root) = &scan_root {
            for file in payload.file_to_symbols.keys() {
                file_roots.insert(file.clone(), root.clone());
            }
        }

        let mut interface_implementors = payload.interface_implementors.clone();
        for (child, parents) in &payload.type_hierarchy {
            for parent in parents {
                let list = interface_implementors.entry(parent.clone()).or_default();
                if !list.iter().any(|c| c == child) {
                    list.push(child.clone());
                }
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
            type_hierarchy: RwLock::new(payload.type_hierarchy),
            interface_implementors: RwLock::new(interface_implementors),
            forgotten_symbols: RwLock::new(HashSet::new()),
            forgotten_files: RwLock::new(HashSet::new()),
            scan_root: RwLock::new(scan_root),
            file_roots: RwLock::new(file_roots),
            symbol_to_file: RwLock::new(symbol_to_file),
            // Both are scan-scoped. A loaded index already carries the edges
            // they were used to produce, in the nodes' own dependencies.
            symbol_lines: RwLock::new(HashMap::new()),
            pending_refs: RwLock::new(HashMap::new()),
        })
    }
}

/// Zoekt Trigram-based In-Memory Search Engine
///
/// A posting is a path id, not the path string. `add_document` used to insert
/// `path.to_string()` into a set once per trigram, which is once per byte of
/// the file: a megabyte of source meant a million path allocations, paid again
/// on every server start, because the trigram index is rebuilt from the source
/// on load. The path is interned to a `u32` once and the postings hold that.
pub struct ZoektIndex {
    /// Interned path for each id, indexed by the id.
    paths: Vec<String>,
    /// The id assigned to each path.
    path_ids: HashMap<String, u32>,
    /// The source of each path, kept for reading the matching line back out.
    contents: HashMap<u32, String>,
    /// Trigram to the ids of the documents that contain it.
    trigrams: HashMap<[u8; 3], HashSet<u32>>,
}

impl Default for ZoektIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoektIndex {
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            path_ids: HashMap::new(),
            contents: HashMap::new(),
            trigrams: HashMap::new(),
        }
    }

    /// The id for a path, assigning one the first time it is seen.
    fn intern(&mut self, path: &str) -> u32 {
        if let Some(id) = self.path_ids.get(path) {
            return *id;
        }
        let id = self.paths.len() as u32;
        self.paths.push(path.to_string());
        self.path_ids.insert(path.to_string(), id);
        id
    }

    pub fn add_document(&mut self, path: &str, content: &str) {
        let id = self.intern(path);
        self.contents.insert(id, content.to_string());
        let bytes = content.as_bytes();
        if bytes.len() >= 3 {
            for i in 0..bytes.len() - 2 {
                let tri = [bytes[i], bytes[i + 1], bytes[i + 2]];
                // `id` is a Copy u32, so this inserts no allocation per byte.
                self.trigrams.entry(tri).or_default().insert(id);
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
        let all_ids = || -> Vec<u32> { (0..self.paths.len() as u32).collect() };
        let candidates: Vec<u32> = if compiled.is_some() {
            all_ids()
        } else if query_bytes.len() >= 3 {
            let mut candidate_set: Option<HashSet<u32>> = None;
            let mut missing_trigram = false;
            for i in 0..query_bytes.len() - 2 {
                let tri = [query_bytes[i], query_bytes[i + 1], query_bytes[i + 2]];
                match self.trigrams.get(&tri) {
                    Some(set) => {
                        if let Some(ref mut c) = candidate_set {
                            c.retain(|id| set.contains(id));
                            if c.is_empty() {
                                missing_trigram = true;
                                break;
                            }
                        } else {
                            candidate_set = Some(set.clone());
                        }
                    }
                    None => {
                        missing_trigram = true;
                        break;
                    }
                }
            }
            if missing_trigram {
                Vec::new()
            } else {
                match candidate_set {
                    Some(c) => c.into_iter().collect(),
                    None => all_ids(),
                }
            }
        } else {
            all_ids()
        };

        for id in candidates {
            if let Some(content) = self.contents.get(&id) {
                for (line_no, line) in content.lines().enumerate() {
                    let hit = match compiled {
                        Some(re) => re.is_match(line),
                        None => line.contains(query),
                    };
                    if hit {
                        matches.push(ZoektMatch {
                            match_kind: "text".to_string(),
                            file_path: self.paths[id as usize].clone(),
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub causal_paths: HashMap<String, Vec<String>>,
    pub total_tests_in_repo: usize,
    pub pruned_test_percentage: f64,
}

/// A token-budgeted adaptive symbol slice for lean context windows
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolContextSlice {
    pub symbol: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    pub estimated_tokens: usize,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
    pub dependencies: Vec<String>,
    pub truncated: bool,
    pub rendered_slice: String,
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

#[cfg(test)]
mod stripping {
    use super::AstIndex;

    /// Stripping must not move a line.
    ///
    /// Two things read the result by line number: `record_references` charges a
    /// call to the last symbol declared above it, and `parse_rust_content`
    /// counts braces to know which type a method belongs to. A swallowed
    /// newline files everything below it under the wrong symbol, silently and
    /// for the rest of the file.
    #[test]
    fn stripping_preserves_every_line() {
        let cases: [(&str, bool, &str); 5] = [
            (
                "let e = format!(\n    \"one \\n     two\"\n);\nfn after() {}\n",
                false,
                "a string continued with a backslash escapes the newline",
            ),
            (
                "struct Holder<'a> {\n    inner: &'a str,\n}\nfn after() {}\n",
                false,
                "a lifetime opens a quote that never closes",
            ),
            (
                "/* block\n   comment */\nfn after() {}\n",
                false,
                "a block comment spans lines",
            ),
            (
                "let s = \"a\nb\";\nfn after() {}\n",
                false,
                "a string holding a real newline",
            ),
            (
                "x = 'it is a string here'\ndef after():\n    pass\n",
                true,
                "an apostrophe delimits a string in Python",
            ),
        ];

        for (src, single_quotes_are_strings, why) in cases {
            let clean = AstIndex::strip_comments_and_strings(src, single_quotes_are_strings);
            assert_eq!(
                clean.split('\n').count(),
                src.split('\n').count(),
                "{why}: stripping moved a line in {src:?} -> {clean:?}"
            );
        }
    }

    /// The same invariant against real source rather than a fixture, because
    /// the shapes that swallow a newline are the shapes real code has: the
    /// backslash continuation this pins was found here, not in a fixture.
    #[test]
    fn stripping_preserves_every_line_of_this_file() {
        let src = include_str!("lib.rs");
        let clean = AstIndex::strip_comments_and_strings(src, false);
        assert_eq!(
            clean.split('\n').count(),
            src.split('\n').count(),
            "stripping this crate's own source moved a line"
        );
    }

    #[test]
    fn zoekt_trigram_prunes_missing_trigrams_without_scanning() {
        let mut zoekt = super::ZoektIndex::new();
        zoekt.add_document("file1.rs", "fn compute_magic_number() -> i32 { 42 }\n");
        zoekt.add_document("file2.rs", "fn other_function() -> bool { true }\n");

        // Literal query with absent trigram should return empty results instantly
        let results = zoekt.search("nonexistent_symbol_xyz", None, 10);
        assert!(results.is_empty());

        // Literal query present in file1
        let results = zoekt.search("compute_magic_number", None, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "file1.rs");
    }
}
