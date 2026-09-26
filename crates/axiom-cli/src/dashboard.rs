//! `axiom dashboard`: what agents are doing to the workspace, on one screen.
//!
//! Three panels, each read from a file the workspace already keeps:
//!
//! - **Writes**, from `.axiom/source_writes.jsonl`: which symbols agents wrote
//!   with `axiom_apply_mutation write_source`, how many agents touched each,
//!   and where a write was merged with another agent's change or refused as a
//!   conflict. A refusal leaves nothing in the file, the index or the op log,
//!   so this log is the only place a collision is visible afterwards.
//! - **Ledger**, from `.axiom/attestations.json`: whether each record still
//!   names the seal of the one before it, who signed it, and what checked the
//!   change. It checks links, not seals: a seal is recomputed from the prompt
//!   it was issued for, which the ledger does not hold, so that check stays
//!   with `axiom verify --prompt`.
//! - **Blast**, with `--symbol`: the tests the symbol reaches, drawn as the
//!   paths that lead to them, from the same `paths_to_tests` the DOT export
//!   uses.
//!
//! `gather` reads the world and `render` is pure, so the frame a test checks is
//! the frame a terminal shows. In a terminal it redraws until interrupted; in
//! a pipe it prints one frame without colour.

use crate::export::paths_to_tests;
use axiom_ast::AstIndex;
use axiom_core::mcp::SourceWrite;
use axiom_proto::ProvenanceAttestation;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const RED: &str = "31";
const GREEN: &str = "32";
const YELLOW: &str = "33";
const DIM: &str = "90";
const BOLD: &str = "1";

/// Rows shown per panel before the rest is counted instead.
const WRITE_ROWS: usize = 12;
const LEDGER_ROWS: usize = 8;
const TREE_ROWS: usize = 24;

/// Everything one frame shows, read once.
pub struct View {
    pub workspace: String,
    pub clock: String,
    pub now: i64,
    pub symbols: usize,
    pub tests: usize,
    pub merkle_root: String,
    pub index_age: Option<i64>,
    pub writes: Vec<SourceWrite>,
    pub ledger: Result<Vec<ProvenanceAttestation>, String>,
    pub blast: Option<Result<Value, String>>,
}

/// Read the workspace behind `axiom_dir` into a view.
pub fn gather(index: &AstIndex, axiom_dir: &Path, symbol: Option<&str>, depth: usize) -> View {
    let now = chrono::Local::now();
    let index_age = std::fs::metadata(axiom_dir.join("index.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() as i64);
    let blast = symbol.map(|s| match index.compute_blast_radius(s, depth) {
        Some(answer) => serde_json::to_value(answer).map_err(|e| e.to_string()),
        None => {
            let candidates = index.candidates_for(s);
            Err(if candidates.len() > 1 {
                format!("{s} matches {} symbols; name one of them", candidates.len())
            } else {
                format!("{s} is not in the index")
            })
        }
    });
    View {
        workspace: axiom_dir
            .parent()
            .unwrap_or(axiom_dir)
            .display()
            .to_string(),
        clock: now.format("%H:%M:%S").to_string(),
        now: now.timestamp(),
        symbols: index.total_symbols_count(),
        tests: index.total_tests_count(),
        merkle_root: index.compute_merkle_root(),
        index_age,
        writes: axiom_core::mcp::load_source_writes(&axiom_dir.join("source_writes.jsonl")),
        ledger: axiom_core::mcp::load_attestations_from(&axiom_dir.join("attestations.json"))
            .map_err(|e| e.to_string()),
        blast,
    }
}

/// Lines under construction, fitted to a width, coloured or not.
struct Frame {
    width: usize,
    color: bool,
    lines: Vec<String>,
}

impl Frame {
    fn paint(&self, s: &str, code: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// A line of plain text, cut to the width and then painted whole.
    fn text(&mut self, s: &str, code: Option<&str>) {
        let fitted = fit(s, self.width);
        let line = match code {
            Some(c) => self.paint(&fitted, c),
            None => fitted,
        };
        self.lines.push(line);
    }

    /// A row led by a coloured marker, its text cut to what is left.
    fn row(&mut self, marker: &str, code: &str, s: &str) {
        let fitted = fit(s, self.width.saturating_sub(4));
        let line = format!("  {} {fitted}", self.paint(marker, code));
        self.lines.push(line);
    }

    fn blank(&mut self) {
        self.lines.push(String::new());
    }
}

/// Cut `s` to `width` characters, marking the cut.
fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn ago(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The frame for `view`, `width` columns wide. `refresh` names the interval
/// for a live frame, whose footer says how to leave; `None` is a single frame.
pub fn render(view: &View, width: usize, color: bool, refresh: Option<u64>) -> String {
    let mut f = Frame {
        width,
        color,
        lines: Vec::new(),
    };

    let title = format!(" axiom  {}", view.workspace);
    let gap = width.saturating_sub(title.chars().count() + view.clock.len() + 1);
    let head = format!("{}{}{} ", fit(&title, width), " ".repeat(gap), view.clock);
    f.lines.push(f.paint(&fit(&head, width), BOLD));
    if view.symbols == 0 {
        f.text(
            " no index here: run `axiom scan --path .` first",
            Some(YELLOW),
        );
    } else {
        let root: String = view.merkle_root.chars().take(12).collect();
        let age = view
            .index_age
            .map(|a| format!(" · index scanned {}", ago(a)))
            .unwrap_or_default();
        f.text(
            &format!(
                " {} · {} · root {root}{age}",
                plural(view.symbols, "symbol"),
                plural(view.tests, "test")
            ),
            Some(DIM),
        );
    }

    f.blank();
    writes_panel(&mut f, view);
    f.blank();
    ledger_panel(&mut f, view);
    if let Some(blast) = &view.blast {
        f.blank();
        blast_panel(&mut f, blast);
    }

    f.blank();
    const LEGEND: usize = " ● open conflict  ● several agents  ● one agent".len() - 6;
    let legend = format!(
        " {} open conflict  {} several agents  {} one agent",
        f.paint("●", RED),
        f.paint("●", YELLOW),
        f.paint("●", GREEN)
    );
    let mut rest = String::new();
    if view.blast.is_none() {
        rest.push_str("  ·  --symbol <SYM> adds its blast radius");
    }
    if let Some(secs) = refresh {
        rest.push_str(&format!("  ·  every {secs}s, ctrl-c quits"));
    }
    let rest = fit(&rest, width.saturating_sub(LEGEND));
    f.lines.push(format!("{legend}{}", f.paint(&rest, DIM)));

    f.lines.join("\n") + "\n"
}

/// One symbol's writes, summed over the log.
#[derive(Default)]
struct Touched {
    agents: Vec<String>,
    written: usize,
    merged: usize,
    conflicts: usize,
    last_at: u64,
    /// Each agent's latest outcome on this symbol. An agent whose latest is a
    /// conflict has a change that never landed, whatever others wrote since.
    latest: BTreeMap<String, String>,
}

impl Touched {
    fn waiting(&self) -> Vec<&str> {
        self.latest
            .iter()
            .filter(|(_, outcome)| *outcome == "conflict")
            .map(|(agent, _)| agent.as_str())
            .collect()
    }
}

fn writes_panel(f: &mut Frame, view: &View) {
    if view.writes.is_empty() {
        f.text(
            "WRITES  no source writes yet. Agents write with axiom_apply_mutation write_source: true",
            Some(DIM),
        );
        return;
    }

    let mut by_symbol: BTreeMap<&str, Touched> = BTreeMap::new();
    let mut everyone: BTreeSet<&str> = BTreeSet::new();
    for w in &view.writes {
        let t = by_symbol.entry(&w.symbol).or_default();
        if !t.agents.contains(&w.agent) {
            t.agents.push(w.agent.clone());
        }
        everyone.insert(&w.agent);
        match w.outcome.as_str() {
            "conflict" => t.conflicts += 1,
            "merged" => t.merged += 1,
            _ => t.written += 1,
        }
        // The log is appended under the source-write lock, so its order is
        // the order the writes happened in and the last entry per agent wins.
        t.last_at = t.last_at.max(w.at);
        t.latest.insert(w.agent.clone(), w.outcome.clone());
    }

    let landed = view
        .writes
        .iter()
        .filter(|w| w.outcome != "conflict")
        .count();
    let contended = by_symbol.values().filter(|t| t.agents.len() > 1).count();
    let conflicts: usize = by_symbol.values().map(|t| t.conflicts).sum();
    let waiting: usize = by_symbol.values().map(|t| t.waiting().len()).sum();
    let header = format!(
        "WRITES  {} · {} landed · {} contended · {}{}",
        plural(everyone.len(), "agent"),
        plural(landed, "write"),
        plural(contended, "symbol"),
        plural(conflicts, "conflict"),
        if waiting > 0 {
            format!(", {} not landed", plural(waiting, "change"))
        } else {
            String::new()
        }
    );
    f.text(&header, Some(BOLD));

    let mut rows: Vec<(&str, Touched)> = by_symbol.into_iter().collect();
    rows.sort_by(|a, b| b.1.last_at.cmp(&a.1.last_at).then(a.0.cmp(b.0)));
    let shown = rows.len().min(WRITE_ROWS);
    for (symbol, t) in rows.iter().take(shown) {
        let waiting = t.waiting();
        // The state goes first after the symbol: it is what the row is for, and
        // the end of a row is what the width cuts.
        let (code, state) = if !waiting.is_empty() {
            (RED, format!("  not landed: {}", waiting.join(", ")))
        } else if t.agents.len() > 1 {
            (YELLOW, String::new())
        } else {
            (GREEN, String::new())
        };
        let names = if t.agents.len() > 3 {
            format!("{} +{}", t.agents[..3].join(", "), t.agents.len() - 3)
        } else {
            t.agents.join(", ")
        };
        let mut counts = vec![format!("{} written", t.written)];
        if t.merged > 0 {
            counts.push(format!("{} merged", t.merged));
        }
        if t.conflicts > 0 {
            counts.push(plural(t.conflicts, "conflict"));
        }
        f.row(
            "●",
            code,
            &format!(
                "{symbol}{state}  {names}  {}  {}",
                counts.join(", "),
                ago(view.now - t.last_at as i64)
            ),
        );
    }
    if rows.len() > shown {
        f.text(
            &format!("    … and {} more", plural(rows.len() - shown, "symbol")),
            Some(DIM),
        );
    }
}

fn ledger_panel(f: &mut Frame, view: &View) {
    let records = match &view.ledger {
        Err(e) => {
            f.text(&format!("LEDGER  unreadable: {e}"), Some(RED));
            return;
        }
        Ok(r) if r.is_empty() => {
            f.text(
                "LEDGER  no records yet. axiom_attest_commit writes them",
                Some(DIM),
            );
            return;
        }
        Ok(r) => r,
    };

    // Whether each record names the seal of the one before it, as
    // `verify_chain` asks, but per record, so the panel can say where.
    let linked: Vec<bool> = records
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i == 0 {
                r.previous_seal.is_empty()
            } else {
                r.previous_seal == records[i - 1].seal
            }
        })
        .collect();
    let first_break = linked.iter().position(|ok| !ok);

    let signers: BTreeSet<&str> = records
        .iter()
        .filter(|r| !r.public_key.is_empty())
        .map(|r| r.public_key.as_str())
        .collect();
    let signed = records.iter().filter(|r| !r.signature.is_empty()).count();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        *kinds.entry(r.verified_by.as_str()).or_default() += 1;
    }
    let kinds: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
    let chain = match first_break {
        None => "chain intact".to_string(),
        Some(i) => format!("chain breaks at #{}", i + 1),
    };
    let signing = if signed == 0 {
        "none signed".to_string()
    } else {
        format!("{signed} signed by {}", plural(signers.len(), "key"))
    };
    let header = format!(
        "LEDGER  {} · {chain} · {signing} · {}",
        plural(records.len(), "record"),
        kinds.join(", ")
    );
    f.text(
        &header,
        Some(if first_break.is_some() { RED } else { BOLD }),
    );

    // The newest records, and the first break wherever it is.
    let mut shown: BTreeSet<usize> =
        (records.len().saturating_sub(LEDGER_ROWS)..records.len()).collect();
    if let Some(i) = first_break {
        shown.insert(i);
    }
    let hidden = records.len() - shown.len();
    if hidden > 0 {
        f.text(
            &format!("    … {} not shown", plural(hidden, "earlier record")),
            Some(DIM),
        );
    }
    for i in shown {
        let r = &records[i];
        let signer = if r.public_key.is_empty() {
            "unsigned".to_string()
        } else {
            format!(
                "signed {}",
                axiom_proto::signing::fingerprint(&r.public_key)
            )
        };
        let when = chrono::DateTime::parse_from_rfc3339(&r.timestamp)
            .map(|t| ago(view.now - t.timestamp()))
            .unwrap_or_else(|_| r.timestamp.clone());
        let (marker, code) = if linked[i] {
            ("✓", GREEN)
        } else {
            ("✗", RED)
        };
        f.row(
            marker,
            code,
            &format!(
                "#{}  {}  {}  {signer}  {}  {when}",
                i + 1,
                r.symbol_path,
                r.verified_by,
                r.agent_identity
            ),
        );
        if !linked[i] {
            let named = short_seal(&r.previous_seal);
            let why = if i == 0 {
                format!(
                    "      names predecessor {named}, which is not in the ledger: records before it were removed"
                )
            } else {
                format!(
                    "      names predecessor {named}, but #{i} seals as {}: a record was removed or reordered",
                    short_seal(&records[i - 1].seal)
                )
            };
            f.text(&why, Some(RED));
        }
    }
    f.text(
        "    links are checked here; a seal needs its prompt: axiom verify --symbol S --prompt P",
        Some(DIM),
    );
}

/// Enough of a seal to tell two apart. Every seal starts with the same
/// `blake3_seal_` label, so its first characters say nothing; the digest does.
fn short_seal(seal: &str) -> String {
    if seal.is_empty() {
        return "(none)".to_string();
    }
    let digest = seal.rsplit('_').next().unwrap_or(seal);
    digest.chars().take(10).collect()
}

/// A tree of paths sharing prefixes, children in name order.
#[derive(Default)]
struct Branch {
    children: BTreeMap<String, Branch>,
}

fn blast_panel(f: &mut Frame, blast: &Result<Value, String>) {
    let answer = match blast {
        Err(e) => {
            f.text(&format!("BLAST  {e}"), Some(RED));
            return;
        }
        Ok(a) => a,
    };
    let symbol = answer["symbol"].as_str().unwrap_or("?");
    let paths = paths_to_tests(answer);
    let total = answer["total_tests_in_repo"].as_u64().unwrap_or(0);
    let pruned = answer["pruned_test_percentage"].as_f64().unwrap_or(0.0);
    f.text(
        &format!(
            "BLAST  {symbol}  {} of {} · {pruned:.1}% pruned",
            paths.len(),
            plural(total as usize, "test")
        ),
        Some(BOLD),
    );
    if paths.is_empty() {
        f.text(
            "    no test reaches it at this depth. That is not the same as nothing being affected",
            Some(DIM),
        );
        return;
    }

    let tests: BTreeSet<&str> = paths
        .iter()
        .filter_map(|p| p.last().map(String::as_str))
        .collect();
    let mut root = Branch::default();
    for path in &paths {
        let mut at = &mut root;
        for step in path.iter().skip(1) {
            at = at.children.entry(step.clone()).or_default();
        }
    }

    f.text(&format!("  {}", name_first(symbol)), None);
    let mut drawn = Vec::new();
    draw(&root, "  ", &tests, &mut drawn);
    let shown = drawn.len().min(TREE_ROWS);
    for (line, is_test) in drawn.iter().take(shown) {
        f.text(line, if *is_test { Some(GREEN) } else { None });
    }
    if drawn.len() > shown {
        f.text(
            &format!("    … and {} more", plural(drawn.len() - shown, "line")),
            Some(DIM),
        );
    }

    let depth_layers = answer["tests_by_depth"].as_object();
    let deeper: Vec<(u64, usize)> = depth_layers
        .map(|layers| {
            let mut rows: Vec<(u64, usize)> = layers
                .iter()
                .filter_map(|(d, v)| Some((d.parse().ok()?, v.as_array()?.len())))
                .collect();
            rows.sort();
            rows
        })
        .unwrap_or_default();
    let listed = paths.len();
    let beyond: usize = deeper
        .iter()
        .map(|(_, n)| n)
        .sum::<usize>()
        .saturating_sub(listed);
    if beyond > 0 {
        let widest = deeper.last().map(|(d, _)| *d).unwrap_or(0);
        f.text(
            &format!(
                "    {} more reach it deeper; --depth {widest} includes them",
                plural(beyond, "test")
            ),
            Some(DIM),
        );
    }
}

/// A symbol with its own name first and where it lives after, so a line cut to
/// the width loses the location rather than the name: `src/lib.rs::helper`
/// reads `helper  src/lib.rs`, `pkg.Gate::isOpen` reads `isOpen  pkg.Gate`.
fn name_first(symbol: &str) -> String {
    match symbol.split_once("::") {
        Some((home, name)) if !name.is_empty() => format!("{name}  {home}"),
        _ => symbol.to_string(),
    }
}

fn draw(branch: &Branch, prefix: &str, tests: &BTreeSet<&str>, out: &mut Vec<(String, bool)>) {
    let count = branch.children.len();
    for (i, (name, child)) in branch.children.iter().enumerate() {
        let last = i + 1 == count;
        out.push((
            format!(
                "{prefix}{}{}",
                if last { "└─ " } else { "├─ " },
                name_first(name)
            ),
            tests.contains(name.as_str()),
        ));
        let deeper = format!("{prefix}{}", if last { "   " } else { "│  " });
        draw(child, &deeper, tests, out);
    }
}
