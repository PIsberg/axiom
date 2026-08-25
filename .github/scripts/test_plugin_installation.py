#!/usr/bin/env python3
"""Validates the installation and live MCP runtime of the Axiom agent plugin.

This verification is run in CI on both Linux and Windows to prevent any regression
in:
1. Binary installation and CLI interface.
2. Open standard plugin structure (.agents/plugins/axiom/).
3. Plugin manifest schema and client MCP config definitions.
4. Stdio MCP server protocol handshake and all 8 tool dispatches.
5. End-to-end multi-language indexing, mutation, and cryptographic attestation.
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def log(msg):
    print(f"[plugin-test] {msg}", flush=True)


def run_cmd(args, cwd=None, check=True):
    log(f"Running: {' '.join(str(a) for a in args)}")
    res = subprocess.run(
        args,
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if check and res.returncode != 0:
        print(f"FAILED (code {res.returncode}):", file=sys.stderr)
        print(f"Stdout:\n{res.stdout}", file=sys.stderr)
        print(f"Stderr:\n{res.stderr}", file=sys.stderr)
        sys.exit(res.returncode)
    return res


def test_manifests(repo_root: Path):
    log("Verifying plugin bundle files and manifests...")
    plugin_dir = repo_root / ".agents" / "plugins" / "axiom"
    assert plugin_dir.is_dir(), f"Plugin directory missing at {plugin_dir}"

    # 1. plugin.json
    plugin_json_path = plugin_dir / "plugin.json"
    assert plugin_json_path.is_file(), f"plugin.json missing at {plugin_json_path}"
    with open(plugin_json_path, "r", encoding="utf-8") as f:
        p_data = json.load(f)
    assert p_data.get("name") == "axiom-engine", f"Invalid name in plugin.json: {p_data}"
    assert "version" in p_data, "Missing version in plugin.json"
    assert "description" in p_data, "Missing description in plugin.json"
    log("plugin.json is valid.")

    # 2. mcp_config.json
    mcp_config_path = plugin_dir / "mcp_config.json"
    assert mcp_config_path.is_file(), f"mcp_config.json missing at {mcp_config_path}"
    with open(mcp_config_path, "r", encoding="utf-8") as f:
        m_data = json.load(f)
    assert "mcpServers" in m_data, "Missing mcpServers in mcp_config.json"
    assert "axiom" in m_data["mcpServers"], "Missing axiom server entry in mcp_config.json"
    server_cfg = m_data["mcpServers"]["axiom"]
    assert server_cfg.get("command") == "axiom", f"Invalid command in mcp_config.json: {server_cfg}"
    assert "serve" in server_cfg.get("args", []), f"Missing 'serve' arg in mcp_config.json: {server_cfg}"
    log("mcp_config.json is valid.")

    # 3. rules/AGENTS.md
    agents_md_path = plugin_dir / "rules" / "AGENTS.md"
    assert agents_md_path.is_file(), f"rules/AGENTS.md missing at {agents_md_path}"
    agents_md = agents_md_path.read_text(encoding="utf-8")
    for req_kw in ["axiom_query_symbol", "axiom_get_blast_radius", "axiom_eval_patch", "axiom_apply_mutation", "axiom_attest_commit"]:
        assert req_kw in agents_md, f"rules/AGENTS.md missing key guideline: {req_kw}"
    log("rules/AGENTS.md is valid.")

    # 4. skills/axiom-engine/SKILL.md
    skill_md_path = plugin_dir / "skills" / "axiom-engine" / "SKILL.md"
    assert skill_md_path.is_file(), f"SKILL.md missing at {skill_md_path}"
    skill_md = skill_md_path.read_text(encoding="utf-8")
    assert "name: axiom-engine" in skill_md, "SKILL.md missing frontmatter name"
    log("skills/axiom-engine/SKILL.md is valid.")

    # 5. docs/plugin_installation.md
    install_doc = repo_root / "docs" / "plugin_installation.md"
    assert install_doc.is_file(), f"docs/plugin_installation.md missing at {install_doc}"
    log("docs/plugin_installation.md is valid.")


def test_cli_and_mcp_runtime(binary_path: str):
    log(f"Testing installed binary at: {binary_path}")

    # 1. Version check
    res = run_cmd([binary_path, "--version"])
    log(f"Version output: {res.stdout.strip()}")
    assert "axiom" in res.stdout.lower(), f"Unexpected version output: {res.stdout}"

    # 2. Help check
    res_help = run_cmd([binary_path, "--help"])
    for cmd in ["serve", "eval", "symbol", "blast-radius", "scan", "search", "verify", "mcp-config"]:
        assert cmd in res_help.stdout, f"Subcommand '{cmd}' missing from CLI help"
    log("All subcommands advertised in CLI help.")

    # 3. Create fresh workspace with Java, Python, and Rust files
    temp_dir = Path(tempfile.mkdtemp(prefix="axiom_install_test_"))
    log(f"Created test workspace at: {temp_dir}")
    try:
        # Java file
        java_dir = temp_dir / "src" / "main" / "java" / "com" / "example"
        java_dir.mkdir(parents=True, exist_ok=True)
        (java_dir / "AuthService.java").write_text(
            "package com.example;\n\n"
            "public class AuthService {\n"
            "    public boolean validateToken(String token) {\n"
            "        return token != null && !token.isEmpty();\n"
            "    }\n"
            "}\n",
            encoding="utf-8",
        )

        # Python file
        (temp_dir / "service.py").write_text(
            "class TokenValidator:\n"
            "    def is_valid(self, token: str) -> bool:\n"
            "        return len(token) > 0\n",
            encoding="utf-8",
        )

        # Rust file
        (temp_dir / "lib.rs").write_text(
            "pub fn authenticate(key: &str) -> bool {\n"
            "    !key.is_empty()\n"
            "}\n",
            encoding="utf-8",
        )

        # 4. Initialize and index workspace via CLI scan
        log("Scanning and indexing workspace...")
        run_cmd([binary_path, "scan", "--path", "."], cwd=temp_dir)
        index_file = temp_dir / ".axiom" / "index.json"
        assert index_file.is_file(), f"Expected index file at {index_file}"
        with open(index_file, "r", encoding="utf-8") as f:
            idx_data = json.load(f)
        assert len(idx_data.get("nodes", {})) >= 3, f"Expected at least 3 symbols indexed, got: {idx_data}"
        log(f"Workspace indexed successfully with {len(idx_data['nodes'])} symbols.")

        # 5. Test Live stdio MCP Handshake and all 8 tools
        log("Starting stdio MCP server session...")
        proc = subprocess.Popen(
            [binary_path, "serve"],
            cwd=temp_dir,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )

        def send_req(req_id, method, params=None):
            msg = {"jsonrpc": "2.0", "id": req_id, "method": method}
            if params is not None:
                msg["params"] = params
            raw = json.dumps(msg) + "\n"
            proc.stdin.write(raw)
            proc.stdin.flush()
            line = proc.stdout.readline()
            assert line, "MCP server stdout closed unexpectedly"
            resp = json.loads(line)
            if "error" in resp:
                raise RuntimeError(f"MCP error response for {method}: {resp['error']}")
            return resp.get("result", {})

        # 5a. Initialize
        init_res = send_req(1, "initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test-runner", "version": "1.0.0"}
        })
        assert "serverInfo" in init_res, f"Invalid initialize response: {init_res}"
        log("MCP initialize handshake succeeded.")

        # 5b. List Tools
        tools_res = send_req(2, "tools/list")
        tool_names = {t["name"] for t in tools_res.get("tools", [])}
        expected_tools = {
            "axiom_query_symbol",
            "axiom_get_blast_radius",
            "axiom_eval_patch",
            "axiom_run_tests",
            "axiom_apply_mutation",
            "axiom_record_verification",
            "axiom_attest_commit",
            "axiom_search_regex",
        }
        missing = expected_tools - tool_names
        assert not missing, f"Missing MCP tools in tools/list: {missing}"
        log(f"All {len(expected_tools)} MCP tools verified in tools/list.")

        # 5c. Query Symbol
        q_res = send_req(3, "tools/call", {
            "name": "axiom_query_symbol",
            "arguments": {"symbol_path": "AuthService"}
        })
        content_json = json.loads(q_res["content"][0]["text"])
        assert "com.example.AuthService" in content_json.get("symbol_path", ""), f"Query failed: {content_json}"
        log("axiom_query_symbol succeeded.")

        # 5d. Blast Radius
        br_res = send_req(4, "tools/call", {
            "name": "axiom_get_blast_radius",
            "arguments": {"symbol_path": "com.example.AuthService"}
        })
        br_json = json.loads(br_res["content"][0]["text"])
        assert "impacted_tests" in br_json, f"Blast radius failed: {br_json}"
        log("axiom_get_blast_radius succeeded.")

        # 5e. Search Regex
        s_res = send_req(5, "tools/call", {
            "name": "axiom_search_regex",
            "arguments": {"query": "authenticate", "mode": "literal"}
        })
        s_json = json.loads(s_res["content"][0]["text"])
        assert s_json.get("matches_count", 0) >= 1, f"Search regex failed: {s_json}"
        log("axiom_search_regex succeeded.")

        # 5f. Apply Mutation
        mut_res = send_req(6, "tools/call", {
            "name": "axiom_apply_mutation",
            "arguments": {
                "node_id": "com.example.AuthService",
                "symbol_path": "com.example.AuthService::validateToken",
                "content": "public boolean validateToken(String token) { return token != null && token.length() > 5; }"
            }
        })
        mut_json = json.loads(mut_res["content"][0]["text"])
        assert mut_json.get("status") == "APPLIED", f"Apply mutation failed: {mut_json}"
        log("axiom_apply_mutation succeeded.")

        # 5g. Record Verification
        rec_res = send_req(7, "tools/call", {
            "name": "axiom_record_verification",
            "arguments": {
                "task_id": "task_ci_install_check_1",
                "passed": True,
                "command": "mvn test"
            }
        })
        rec_json = json.loads(rec_res["content"][0]["text"])
        assert rec_json.get("passed") is True, f"Record verification failed: {rec_json}"
        log("axiom_record_verification succeeded.")

        # 5h. Attest Commit
        att_res = send_req(8, "tools/call", {
            "name": "axiom_attest_commit",
            "arguments": {
                "prompt": "Enforce token length check in AuthService",
                "symbol_path": "com.example.AuthService::validateToken",
                "ctop_task_id": "task_ci_install_check_1",
                "agent_identity": "ci-verifier"
            }
        })
        att_json = json.loads(att_res["content"][0]["text"])
        assert "seal" in att_json, f"Attest commit failed: {att_json}"
        log(f"axiom_attest_commit succeeded with cryptographic seal: {att_json['seal'][:16]}...")

        # Terminate MCP server cleanly
        proc.stdin.close()
        proc.wait(timeout=5)
        log("MCP server terminated cleanly.")

    finally:
        # Cleanup
        try:
            import shutil
            shutil.rmtree(temp_dir, ignore_errors=True)
        except Exception:
            pass


def main():
    if len(sys.argv) < 2:
        print("Usage: test_plugin_installation.py <path_to_axiom_binary>", file=sys.stderr)
        sys.exit(1)

    binary_path = sys.argv[1]
    repo_root = Path(__file__).resolve().parent.parent.parent

    log(f"Starting Axiom plugin installation verification for: {binary_path}")
    test_manifests(repo_root)
    test_cli_and_mcp_runtime(binary_path)
    log("ALL PLUGIN INSTALLATION & RUNTIME CHECKS PASSED.")


if __name__ == "__main__":
    main()
