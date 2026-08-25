# Axiom Plugin & MCP Installation Guide

Axiom can be distributed and installed as an open-standard plugin and Model Context Protocol (MCP) server across AI agent environments including **Antigravity**, **Claude Code**, **Cursor**, **Windsurf**, and **VSCode / Cline**.

---

## 1. Prerequisites: Build the Axiom Binary

Build the release binary and add it to your system `PATH`:

```bash
# Clone and build
cargo build --release --bin axiom

# Linux / macOS: Add to PATH
sudo cp target/release/axiom /usr/local/bin/

# Windows (PowerShell):
Copy-Item target\release\axiom.exe C:\Windows\System32\  # or custom directory in PATH
```

Verify installation:
```bash
axiom --version
```

---

## 2. Antigravity Plugin Installation

Antigravity natively discovers plugins conforming to the open customization standard in `.agents/plugins/` (workspace) and `~/.gemini/config/plugins/` (global).

### Workspace Plugin (Checked into Git)
Place the plugin in `.agents/plugins/axiom/` within your project repository:
```text
.agents/plugins/axiom/
├── plugin.json
├── mcp_config.json
├── rules/
│   └── AGENTS.md
├── skills/
│   └── axiom-engine/
│       └── SKILL.md
└── README.md
```

### Global Plugin (All Projects)
To make Axiom active across all projects on your machine:
```bash
# Linux / macOS
mkdir -p ~/.gemini/config/plugins/axiom
cp -r .agents/plugins/axiom/* ~/.gemini/config/plugins/axiom/

# Windows
mkdir -p $HOME\.gemini\config\plugins\axiom
Copy-Item -Recurse .agents\plugins\axiom\* $HOME\.gemini\config\plugins\axiom\
```

Antigravity will automatically discover the plugin, start `axiom serve` in the background over stdio, and equip the agent with all 8 Axiom tools, rules, and skills.

---

## 3. Claude Code & Claude Desktop Setup

In `~/.claude/mcp.json` (Claude Code) or `claude_desktop_config.json` (Claude Desktop):

```json
{
  "mcpServers": {
    "axiom": {
      "command": "axiom",
      "args": ["serve"]
    }
  }
}
```

Or generate automatically:
```bash
axiom mcp-config > ~/.claude/mcp.json
```

---

## 4. Cursor IDE Setup

In `.cursor/mcp.json` or global `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "axiom": {
      "command": "axiom",
      "args": ["serve"]
    }
  }
}
```

---

## 5. Windsurf / Codeium Cascade Setup

In `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "axiom": {
      "command": "axiom",
      "args": ["serve"]
    }
  }
}
```

---

## 6. Available MCP Tools Reference

Once connected, your agent receives the following 8 tools:

| Tool | Purpose | Primary Inputs |
| :--- | :--- | :--- |
| `axiom_query_symbol` | Query exact AST symbol metadata, declarations & dependencies | `symbol_path` |
| `axiom_get_blast_radius` | The tests that can reach a symbol. How much it prunes depends on the suite: measured medians are 99.8% on a 3,429-test Java tree and 92.5% on a 53-test Rust one | `symbol_path`, `max_depth` |
| `axiom_eval_patch` | Compile and run a snippet in the symbol's own language (Rust, Java, Python, Go, TS/JS, Kotlin, Scala, WebAssembly), or refuse. Sandboxed only for WebAssembly; every other language runs on the host | `symbol_path`, `code_snippet` |
| `axiom_run_tests` | Run targeted test suites and record execution verifications | `command`, `task_id`, `symbol_path` |
| `axiom_record_verification` | Record external check outcomes for provenance | `task_id`, `passed`, `command` |
| `axiom_apply_mutation` | Tree-CRDT atomic symbol patch preventing textual merge conflicts | `node_id`, `symbol_path`, `content` |
| `axiom_attest_commit` | Seal cryptographically signed Ed25519 provenance attestations | `prompt`, `symbol_path`, `ctop_task_id`, `agent_identity` |
| `axiom_search_regex` | Zoekt-style trigram literal and regex full-text search | `query`, `mode`, `max_results` |
