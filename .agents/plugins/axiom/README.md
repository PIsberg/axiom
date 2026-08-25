# Axiom Engine Plugin

Open-standard agent plugin for Antigravity, Claude Code, Cursor, Windsurf, and Model Context Protocol (MCP) clients.

## Features
- **In-Memory Merkle AST CAS**: Sub-millisecond symbol queries, signatures, and dependency tracking.
- **Topological Blast Radius**: 80–99% test pruning by targeting only direct and transitive dependents.
- **Sub-Second Multi-Language Sandbox**: Instant evaluation for Java, Rust, Python, TypeScript, JavaScript, Go, Kotlin, Scala, and WASM.
- **Tree-CRDT Multi-Agent Mutation**: Conflict-free concurrent multi-agent swarms.
- **Cryptographic Provenance**: Ed25519-signed attestations linking prompt, symbol, and execution verification.

## Installation

### 1. Build & Install Binary
Ensure `axiom` binary is compiled and available on your system `PATH`:
```bash
cargo build --release --bin axiom
# Copy or symlink target/release/axiom to your PATH (e.g. /usr/local/bin or C:\tools)
```

### 2. Antigravity Installation
Copy this plugin directory into your project's `.agents/plugins/` or global `~/.gemini/config/plugins/`:
```bash
mkdir -p .agents/plugins/axiom
cp -r .agents/plugins/axiom/* .agents/plugins/axiom/
```
Antigravity automatically discovers the plugin, registers `mcp_config.json`, and loads rules & skills.

### 3. Cursor / Claude Code / Windsurf Setup
Add Axiom to your client's MCP configuration (`mcp.json` or `claude_desktop_config.json`):
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
Or run `axiom mcp-config > mcp.json` to generate the exact config for your system.
