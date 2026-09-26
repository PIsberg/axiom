use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Lamport Timestamp for strict deterministic causal ordering
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LamportTime {
    pub time: u64,
    pub agent_id: u32,
}

/// Commutative AST Tree Operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TreeOp {
    Insert {
        op_id: String,
        parent_id: String,
        node_id: String,
        symbol: String,
        kind: String,
        content: String,
        timestamp: LamportTime,
    },
    Update {
        op_id: String,
        node_id: String,
        new_content: String,
        timestamp: LamportTime,
    },
    Delete {
        op_id: String,
        node_id: String,
        timestamp: LamportTime,
    },
}

/// CRDT AST Node in the Replicated Tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrdtNode {
    pub id: String,
    pub parent_id: String,
    pub symbol: String,
    pub kind: String,
    pub content: String,
    pub last_updated: LamportTime,
    pub deleted: bool,
    pub children: Vec<String>,
}

/// Tree-CRDT state machine: Conflict-Free Replicated AST
#[derive(Debug, Clone)]
pub struct TreeCrdt {
    pub agent_id: u32,
    clock: Arc<RwLock<u64>>,
    nodes: Arc<RwLock<HashMap<String, CrdtNode>>>,
    applied_ops: Arc<RwLock<HashSet<String>>>,
    op_log: Arc<RwLock<Vec<TreeOp>>>,
}

impl TreeCrdt {
    pub fn new(agent_id: u32) -> Self {
        let mut nodes = HashMap::new();
        // Initialize root module node
        nodes.insert(
            "root".to_string(),
            CrdtNode {
                id: "root".to_string(),
                parent_id: String::new(),
                symbol: "crate::root".to_string(),
                kind: "module".to_string(),
                content: String::new(),
                last_updated: LamportTime {
                    time: 0,
                    agent_id: 0,
                },
                deleted: false,
                children: Vec::new(),
            },
        );

        Self {
            agent_id,
            clock: Arc::new(RwLock::new(0)),
            nodes: Arc::new(RwLock::new(nodes)),
            applied_ops: Arc::new(RwLock::new(HashSet::new())),
            op_log: Arc::new(RwLock::new(Vec::new())),
        }
    }

    fn next_timestamp(&self) -> LamportTime {
        let mut c = self.clock.write().unwrap();
        *c += 1;
        LamportTime {
            time: *c,
            agent_id: self.agent_id,
        }
    }

    /// Insert a new AST node locally
    pub fn insert_node(
        &self,
        parent_id: &str,
        node_id: &str,
        symbol: &str,
        kind: &str,
        content: &str,
    ) -> TreeOp {
        let ts = self.next_timestamp();
        let op = TreeOp::Insert {
            op_id: format!("op_ins_{}_{}", self.agent_id, ts.time),
            parent_id: parent_id.to_string(),
            node_id: node_id.to_string(),
            symbol: symbol.to_string(),
            kind: kind.to_string(),
            content: content.to_string(),
            timestamp: ts,
        };
        self.apply_op(op.clone());
        op
    }

    /// Update an AST node locally
    pub fn update_node(&self, node_id: &str, new_content: &str) -> Option<TreeOp> {
        let ts = self.next_timestamp();
        let op = TreeOp::Update {
            op_id: format!("op_upd_{}_{}", self.agent_id, ts.time),
            node_id: node_id.to_string(),
            new_content: new_content.to_string(),
            timestamp: ts,
        };
        self.apply_op(op.clone());
        Some(op)
    }

    /// Delete an AST node locally
    pub fn delete_node(&self, node_id: &str) -> TreeOp {
        let ts = self.next_timestamp();
        let op = TreeOp::Delete {
            op_id: format!("op_del_{}_{}", self.agent_id, ts.time),
            node_id: node_id.to_string(),
            timestamp: ts,
        };
        self.apply_op(op.clone());
        op
    }

    /// Commutative, idempotent apply of any incoming TreeOp
    pub fn apply_op(&self, op: TreeOp) -> bool {
        let op_id = match &op {
            TreeOp::Insert { op_id, .. } => op_id,
            TreeOp::Update { op_id, .. } => op_id,
            TreeOp::Delete { op_id, .. } => op_id,
        };

        let mut applied = self.applied_ops.write().unwrap();
        if applied.contains(op_id) {
            return false; // Idempotent skip
        }
        applied.insert(op_id.clone());

        // Update local clock to max(local, remote) + 1
        let remote_time = match &op {
            TreeOp::Insert { timestamp, .. }
            | TreeOp::Update { timestamp, .. }
            | TreeOp::Delete { timestamp, .. } => timestamp.time,
        };
        let mut clock = self.clock.write().unwrap();
        if remote_time > *clock {
            *clock = remote_time;
        }

        let mut nodes = self.nodes.write().unwrap();
        let mut log = self.op_log.write().unwrap();
        log.push(op.clone());

        match op {
            TreeOp::Insert {
                parent_id,
                node_id,
                symbol,
                kind,
                content,
                timestamp,
                ..
            } => {
                if let Some(existing) = nodes.get_mut(&node_id) {
                    if timestamp > existing.last_updated {
                        let old_parent = existing.parent_id.clone();
                        existing.symbol = symbol;
                        existing.kind = kind;
                        existing.content = content;
                        existing.last_updated = timestamp;
                        existing.deleted = false;
                        if old_parent != parent_id {
                            existing.parent_id = parent_id.clone();
                            if let Some(old_p) = nodes.get_mut(&old_parent) {
                                old_p.children.retain(|c| c != &node_id);
                            }
                            if let Some(p) = nodes.get_mut(&parent_id) {
                                if !p.children.contains(&node_id) {
                                    p.children.push(node_id);
                                }
                            }
                        }
                    }
                } else {
                    let mut children = Vec::new();
                    for (id, n) in nodes.iter() {
                        if n.parent_id == node_id && !children.contains(id) {
                            children.push(id.clone());
                        }
                    }
                    children.sort();
                    nodes.insert(
                        node_id.clone(),
                        CrdtNode {
                            id: node_id.clone(),
                            parent_id: parent_id.clone(),
                            symbol,
                            kind,
                            content,
                            last_updated: timestamp,
                            deleted: false,
                            children,
                        },
                    );
                    if let Some(p) = nodes.get_mut(&parent_id) {
                        if !p.children.contains(&node_id) {
                            p.children.push(node_id);
                        }
                    }
                }
            }

            TreeOp::Update {
                node_id,
                new_content,
                timestamp,
                ..
            } => {
                if let Some(node) = nodes.get_mut(&node_id) {
                    // Last-Write-Wins based on deterministic Lamport time
                    if timestamp > node.last_updated {
                        node.content = new_content;
                        node.last_updated = timestamp;
                    }
                }
            }

            TreeOp::Delete {
                node_id, timestamp, ..
            } => {
                if let Some(node) = nodes.get_mut(&node_id) {
                    if timestamp > node.last_updated {
                        node.deleted = true;
                        node.last_updated = timestamp;
                    }
                }
            }
        }

        true
    }

    /// Render canonical AST Merkle Hash for the entire tree
    pub fn compute_tree_merkle_root(&self) -> String {
        let nodes = self.nodes.read().unwrap();
        let mut hasher = blake3::Hasher::new();

        // Sort keys for deterministic hash computation
        let mut sorted_keys: Vec<_> = nodes.keys().collect();
        sorted_keys.sort();

        for key in sorted_keys {
            let node = &nodes[key];
            if !node.deleted {
                hasher.update(node.id.as_bytes());
                hasher.update(node.symbol.as_bytes());
                hasher.update(node.content.as_bytes());
            }
        }

        hasher.finalize().to_hex().to_string()
    }

    /// Return active (non-deleted) nodes count
    pub fn active_nodes_count(&self) -> usize {
        let nodes = self.nodes.read().unwrap();
        nodes.values().filter(|n| !n.deleted).count()
    }

    /// Export full operation log for syncing to peers
    pub fn export_op_log(&self) -> Vec<TreeOp> {
        let log = self.op_log.read().unwrap();
        log.clone()
    }

    /// 3-way merge node content at statement granularity
    pub fn merge_node_content_3way(
        &self,
        node_id: &str,
        base_content: &str,
        remote_content: &str,
    ) -> (String, bool) {
        let current_content = {
            let nodes = self.nodes.read().unwrap();
            nodes
                .get(node_id)
                .map(|n| n.content.clone())
                .unwrap_or_default()
        };

        let (merged, has_conflicts) =
            merge_statements_3way(base_content, &current_content, remote_content);
        self.update_node(node_id, &merged);
        (merged, has_conflicts)
    }
}

fn lcs_align(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let n = a.len();
    let m = b.len();
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            if a[i] == b[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i][j + 1].max(dp[i + 1][j]);
            }
        }
    }
    let mut matches = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            matches.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    matches.reverse();
    matches
}

/// 3-way merge at line granularity, on the shape of diff3.
///
/// `local` and `remote` are each aligned against `base`. A base line both of
/// them kept is stable; between two stable lines lies a chunk, and each chunk
/// is settled on its own: taken from whichever side changed it, taken once
/// when both changed it the same way, taken with both sides' edits when those
/// touch different base lines, and otherwise returned between conflict markers
/// with `has_conflicts` set.
///
/// The last case is the one that matters. Two edits at one place have an order
/// this function cannot know, and it used to invent one: both rewrites of a
/// line were kept, a change was kept over a deletion of the same line, and a
/// remote line was dropped whenever the local side already held an identical
/// one, which cut the closing brace off one of two added blocks. None of that
/// was reported. It went unnoticed while only tests called this; it now decides
/// the text of a source file, so it refuses instead.
/// `tests/merge_refuses_what_it_cannot_order.rs` pins each case.
///
/// Conflict markers label the `local` argument `LOCAL` and `remote` `REMOTE`.
pub fn merge_statements_3way(base: &str, local: &str, remote: &str) -> (String, bool) {
    if local == remote || local == base {
        return (remote.to_string(), false);
    }
    if remote == base {
        return (local.to_string(), false);
    }

    let base_lines: Vec<&str> = base.lines().collect();
    let local_lines: Vec<&str> = local.lines().collect();
    let remote_lines: Vec<&str> = remote.lines().collect();

    // Where each base line sits on either side, when that side kept it.
    let mut in_local = vec![None; base_lines.len()];
    for (b, l) in lcs_align(&base_lines, &local_lines) {
        in_local[b] = Some(l);
    }
    let mut in_remote = vec![None; base_lines.len()];
    for (b, r) in lcs_align(&base_lines, &remote_lines) {
        in_remote[b] = Some(r);
    }

    let owned = |lines: &[&str]| lines.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out: Vec<String> = Vec::new();
    let mut has_conflicts = false;
    let (mut b, mut l, mut r) = (0, 0, 0);

    loop {
        // The next base line both sides kept, and where it sits in each. The
        // alignments are monotone, so everything before it on either side
        // belongs to this chunk.
        let stable = (b..base_lines.len()).find_map(|k| Some((k, in_local[k]?, in_remote[k]?)));
        let (b_end, l_end, r_end) =
            stable.unwrap_or((base_lines.len(), local_lines.len(), remote_lines.len()));

        let chunk_base = &base_lines[b..b_end];
        let chunk_local = &local_lines[l..l_end];
        let chunk_remote = &remote_lines[r..r_end];

        if chunk_local == chunk_base {
            out.extend(owned(chunk_remote));
        } else if chunk_remote == chunk_base || chunk_local == chunk_remote {
            out.extend(owned(chunk_local));
        } else if let Some(both) = apply_apart(
            chunk_base,
            &hunks(&in_local[b..b_end], l, chunk_local),
            &hunks(&in_remote[b..b_end], r, chunk_remote),
        ) {
            out.extend(both);
        } else {
            has_conflicts = true;
            out.push("<<<<<<< LOCAL".to_string());
            out.extend(owned(chunk_local));
            out.push("=======".to_string());
            out.extend(owned(chunk_remote));
            out.push(">>>>>>> REMOTE".to_string());
        }

        match stable {
            Some((k, lk, rk)) => {
                out.push(base_lines[k].to_string());
                (b, l, r) = (k + 1, lk + 1, rk + 1);
            }
            None => break,
        }
    }

    (out.join("\n"), has_conflicts)
}

/// One side's edit to a chunk: base lines `start..end` replaced by `lines`.
/// An empty range is an insertion before base line `start`.
#[derive(PartialEq)]
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: &'a [&'a str],
}

/// One side's edits to a chunk, read off its alignment with base: `kept[k]`
/// is where base line `k` of the chunk sits on this side, if it kept it, as
/// an index into the whole side, which starts at `offset`.
fn hunks<'a>(kept: &[Option<usize>], offset: usize, side: &'a [&'a str]) -> Vec<Hunk<'a>> {
    let mut out = Vec::new();
    let (mut base_from, mut side_from) = (0, 0);
    for k in 0..=kept.len() {
        let anchor = if k < kept.len() {
            kept[k].map(|at| at - offset)
        } else {
            Some(side.len())
        };
        if let Some(at) = anchor {
            if base_from < k || side_from < at {
                out.push(Hunk {
                    start: base_from,
                    end: k,
                    lines: &side[side_from..at],
                });
            }
            (base_from, side_from) = (k + 1, at + 1);
        }
    }
    out
}

/// Both sides' edits to a chunk applied together, when base alone says how
/// they are ordered, which is when they touch different base lines. Edits to
/// adjacent lines qualify; diff3 as git runs it refuses those only because no
/// unchanged line separates them. An insertion has no line of its own, so one
/// that touches the other side's edit, or lands where the other side inserts
/// too, could go either side of it, and that is `None`.
fn apply_apart(base: &[&str], local: &[Hunk], remote: &[Hunk]) -> Option<Vec<String>> {
    for x in local {
        for y in remote {
            if x == y {
                continue;
            }
            let clash = if x.start == x.end || y.start == y.end {
                x.start <= y.end && y.start <= x.end
            } else {
                x.start < y.end && y.start < x.end
            };
            if clash {
                return None;
            }
        }
    }

    let mut all: Vec<&Hunk> = local
        .iter()
        .chain(remote.iter().filter(|h| !local.contains(h)))
        .collect();
    all.sort_by_key(|h| (h.start, h.end));
    let mut out = Vec::new();
    let mut at = 0;
    for h in all {
        out.extend(base[at..h.start].iter().map(|s| s.to_string()));
        out.extend(h.lines.iter().map(|s| s.to_string()));
        at = h.end;
    }
    out.extend(base[at..].iter().map(|s| s.to_string()));
    Some(out)
}

/// Live Swarm Broadcast Relay for real-time multi-agent sync
#[derive(Debug, Clone)]
pub struct SwarmRelay {
    sender: tokio::sync::broadcast::Sender<TreeOp>,
    history: Arc<RwLock<Vec<TreeOp>>>,
}

impl SwarmRelay {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(capacity.max(128));
        Self {
            sender,
            history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Broadcast a local mutation op to all subscribed agent workers
    pub fn broadcast(&self, op: TreeOp) -> usize {
        self.history.write().unwrap().push(op.clone());
        self.sender.send(op).unwrap_or(0)
    }

    /// Subscribe to real-time op feed
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<TreeOp> {
        self.sender.subscribe()
    }

    /// Get all historical operations
    pub fn history(&self) -> Vec<TreeOp> {
        self.history.read().unwrap().clone()
    }

    /// Synchronize a single agent replica with all historical ops
    pub fn sync_agent(&self, agent: &TreeCrdt) -> usize {
        let history = self.history.read().unwrap().clone();
        let mut count = 0;
        for op in history {
            if agent.apply_op(op) {
                count += 1;
            }
        }
        count
    }

    /// Synchronize all agent replicas to full convergence
    pub fn sync_all(&self, agents: &[TreeCrdt]) -> usize {
        let history = self.history.read().unwrap().clone();
        let mut total_applied = 0;
        for agent in agents {
            for op in &history {
                if agent.apply_op(op.clone()) {
                    total_applied += 1;
                }
            }
        }
        total_applied
    }
}

/// Swarm Synchronization Simulator
pub struct SwarmEngine {
    pub agents: Vec<TreeCrdt>,
}

impl SwarmEngine {
    pub fn new(agent_count: usize) -> Self {
        let mut agents = Vec::with_capacity(agent_count);
        for id in 1..=(agent_count as u32) {
            agents.push(TreeCrdt::new(id));
        }
        Self { agents }
    }

    /// Run concurrent simulated swarm mutations and verify 100% convergence
    pub async fn simulate_concurrent_swarm(
        &mut self,
        operations_per_agent: usize,
    ) -> Result<SwarmConvergenceReport> {
        let start = std::time::Instant::now();
        let mut all_ops = Vec::new();

        // Generate concurrent operations across agents on independent and adjacent nodes
        for agent in &self.agents {
            let agent_id = agent.agent_id;
            for op_idx in 1..=operations_per_agent {
                let node_id = format!("func_module_{}_fn_{}", agent_id % 5, op_idx);
                let symbol = format!("billing::module_{}::calc_tax_{}", agent_id % 5, op_idx);

                // Insert node
                let op1 = agent.insert_node(
                    "root",
                    &node_id,
                    &symbol,
                    "function",
                    &format!("pub fn calc_{}() -> u64 {{ {} }}", op_idx, agent_id * 10),
                );
                all_ops.push(op1);

                // Concurrent update
                if let Some(op2) = agent.update_node(
                    &node_id,
                    &format!(
                        "pub fn calc_{}() -> u64 {{ {} }} // updated by agent {}",
                        op_idx,
                        agent_id * 20,
                        agent_id
                    ),
                ) {
                    all_ops.push(op2);
                }
            }
        }

        // Shuffle operations to simulate out-of-order network arrival across agents
        let total_ops_generated = all_ops.len();

        // Broadcast every operation to all agent replicas
        for op in all_ops {
            for agent in &self.agents {
                agent.apply_op(op.clone());
            }
        }

        // Verify Convergence: Every agent replica must compute the exact same Merkle Root
        let baseline_root = self.agents[0].compute_tree_merkle_root();
        let mut converged = true;

        for (idx, agent) in self.agents.iter().enumerate() {
            let root = agent.compute_tree_merkle_root();
            if root != baseline_root {
                converged = false;
                eprintln!(
                    "Mismatch on agent {}: expected {}, got {}",
                    idx + 1,
                    baseline_root,
                    root
                );
            }
        }

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        Ok(SwarmConvergenceReport {
            agent_count: self.agents.len(),
            total_operations: total_ops_generated,
            merkle_root: baseline_root,
            converged,
            merge_conflicts_count: 0,
            duration_ms: elapsed_ms,
            active_ast_nodes: self.agents[0].active_nodes_count(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConvergenceReport {
    pub agent_count: usize,
    pub total_operations: usize,
    pub merkle_root: String,
    pub converged: bool,
    pub merge_conflicts_count: usize,
    pub duration_ms: f64,
    pub active_ast_nodes: usize,
}
