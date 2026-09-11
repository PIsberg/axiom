# axiom-core

The MCP server. `src/mcp.rs` (1,973 lines) declares and dispatches every surface the agent sees:
8 tools, 3 prompts, 3 resources. The CLI is not a separate code path; every subcommand constructs
an `AxiomMcpServer` and calls these same functions, so a bug reproduced through
`axiom blast-radius` is the bug an agent sees through `axiom_get_blast_radius`.

## Declared and dispatched are two places that must be edited together

A surface declared but not dispatched fails at call time, not at startup. `tools/list` and the
dispatch `match` below it are the original pair; `prompts/list` with `handle_prompt_get`, and
`resources/list` with `resources/read`, are the same shape.

`tests/declared_tools_are_dispatched.rs` pins the tool set three ways: every declared tool answers
when called, the advertised set is the documented one, and every argument the schema advertises is
one the dispatch reads. The last one matters because the schema is all an agent has to go on. The
schema for `axiom_apply_mutation` once required `symbol` and `kind` while the dispatch read
`symbol_path`; `axiom_run_tests` advertised a `timeout_seconds` nothing read; and
`axiom_attest_commit` listed `ctop_task_id` as optional while the dispatch refused a call without
it. Each of those is answered with a complaint about a name the agent was never shown.

`tests/mcp_resources_prompts.rs` holds the same three properties for prompts and resources: every
declared prompt answers a `prompts/get`, every declared resource answers a `resources/read`, and a
prompt argument marked required is refused when absent rather than rendered as an empty string.
That last one is the tool-schema bug in the prompt surface: a prompt reading a missing required
argument as `""` hands an agent an instruction to refactor a symbol with no name.

## Index discovery walks up from the current directory

`find_index_file` climbs parent directories looking for `.axiom/index.json`. The MCP server
inherits its client's working directory, which is the agent's project, not this repo. A server
that appears to know nothing about the codebase is usually one started somewhere with no index
above it.

## Writes go to the same `.axiom` the read came from

The server records the discovered directory in `axiom_dir` and derives the ledger, the op log and
the mutation index from it, through `ledger_path`, `op_log_path` and `index_path`. Before that,
reads walked up to find the index while every write used `<cwd>/.axiom`, so an agent working from
a subdirectory wrote where the next read would not look. `axiom verify` walks up the same way,
through `find_axiom_dir`.

`axiom scan` and `axiom watch` are the exception on purpose: they anchor to the local
`.axiom/index.json` and nothing above it, because a scan states what one tree contains and must
not fold an ancestor index into it. `tests/server_writes_where_it_reads.rs` and
`axiom-cli/tests/scan_is_anchored.rs` pin both halves.

## Seeding is asked for, not automatic

`AxiomMcpServer::new` once inserted two fixture symbols whenever the index was empty, which made a
workspace nobody had scanned answer confidently about a symbol in no real codebase. That is now
`seed_demo_workspace`, called only by `axiom demo`, so a server with no index above it answers
nothing rather than answering a fixture.

`axiom_query_symbol` returns `total_symbols_in_index` only on its not-found branch, beside the
error; a successful lookup returns `dependencies`, `docstring`, `hash`, `id`, `kind`, `signature`,
`source_range` and `symbol_path` and no count. So the count is there to read when a symbol misses,
which is exactly when telling a real index from an empty one matters, but do not expect it on a
hit. To check the index directly, run `axiom scan` and read the symbol count it prints, or look
for `.axiom/index.json` above the working directory.

## A caller-supplied field that is printed is an injection surface

`agent_identity` reaches `axiom_attest_commit` from the caller and is rendered by `axiom verify`
as one of a column of labelled lines. A value carrying a newline could add lines of its own,
showing a `Checked by: sandbox` line above a record whose `verified_by` says `reported`.
`agent_identity_of` refuses control characters and bounds the length where the value enters,
rather than escaping it at each place it is shown. The same reasoning applies to any future field
that is both caller-set and displayed.

Note what makes storing an unverified name acceptable at all: it is hashed into the seal and
covered by the signature, so it cannot be edited afterwards.

## `axiom_run_tests` is the third verification kind

It runs the project's own test command in the workspace and records the outcome as `executed`:
axiom ran it and saw the exit code, so it can vouch for it. That sits between `sandbox` (axiom's
own evaluator ran it) and `reported` (an agent says it ran something). The command runs with the
confined environment `run_with_timeout` gives every evaluation, so it cannot read the signing key,
and is killed as a whole process tree past `AXIOM_TEST_TIMEOUT_SECS` (default 600, separate from
the evaluator's).

## The docs go stale silently, and tests stop the parts that can be checked

`axiom_run_tests` shipped while the README said seven tools and USAGE_GUIDE.md documented seven;
`cache-validate` shipped and the README's command table listed `cache-audit` alone. Both are
surfaces an agent reads to decide what to call, so a gap there is not untidiness.

`tests/docs_name_every_tool.rs` checks the README and the usage guide against `tools/list`, and
`axiom-cli/tests/readme_lists_every_subcommand.rs` checks the README's command table against clap
in both directions. Code is the source in both; the docs are checked against it, never the
reverse. What no test can check is a number in prose: every measured figure in the README and the
speed report carries the date it was taken, and `.github/scripts/blast_radius_stats.py` exists so
the blast-radius numbers are re-derived rather than remembered.
