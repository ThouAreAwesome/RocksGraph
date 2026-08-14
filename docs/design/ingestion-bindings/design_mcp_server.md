# Design: MCP Server — `rocksgraph-mcp`

Status: proposal — not started.

## Problem

RocksGraph's positioning — a single embedded binary combining ACID graph storage
with integrated HNSW vector search — matches almost exactly what a local,
persistent memory/knowledge backend for an AI agent needs (see
[Autonomous AI Agent Memory](../../ideas/README.md#2-autonomous-ai-agent-memory-semantic-episodic-memory)
and [Local-First Graph RAG](../../ideas/README.md#1-local-first-graph-rag-retrieval-augmented-generation)
in the ideas doc). But there is currently no way for an MCP client — Claude
Desktop, Claude Code, Cursor, Windsurf, or any other Model Context Protocol
client — to actually use a RocksGraph database. Every path to "an agent uses
this data" today requires hand-written integration code (LangChain, LlamaIndex,
or a bespoke script).

MCP is the emerging standard for exactly this connection: a client discovers a
set of `tools` a server exposes and calls them directly, with no
framework-specific glue on the client side. RocksGraph has zero presence there
today (grepped the whole repo for any mention — none).

This also strengthens the [promotion playbook](../project_promotion_playbook.md)'s
existing Phase 3 integration plan: an MCP server reaches Claude Desktop / Claude
Code / Cursor / Windsurf users with one implementation, rather than one
integration per framework, and targets the same "AI/LLM tooling developer"
audience already identified as the overlap across the three High-priority ideas.

## Goals & non-goals

- **Goals:**
  - Ship `rocksgraph-mcp`, a standalone MCP server (own repo, per the promotion
    playbook's existing "Integration template" pattern) wrapping the existing
    Python bindings — no new Rust code required.
  - Expose a **small, curated tool surface**, not raw Gremlin traversal
    execution — see [Design](#design) below for the concrete tool list.
  - **stdio transport only** for v1 — matches how Claude Desktop/Code configure
    local MCP servers today, and matches RocksGraph's own "no daemon, no
    network overhead" positioning. No SSE/HTTP server mode.
  - Support both the Agent Memory and Graph RAG shapes with the *same* generic
    tool set — entities/relationships/vector-search are general enough to
    cover both without needing two servers.
  - Publish to PyPI as `rocksgraph-mcp` and submit to the community MCP
    servers registry, alongside the Awesome-list submissions already planned
    in the promotion playbook's Target Channels.

- **Non-goals (v1):**
  - **Arbitrary Gremlin query execution as a tool.** Handing an LLM caller
    unbounded traversal power is a real cost/safety risk (unbounded hops,
    no query cost limit) — see [Constraints](#constraints--invariants) for
    the bounded alternative. A gated, opt-in escape hatch is a possible v2,
    not the default surface.
  - Multi-graph / multi-tenant serving — one server process holds one open
    `Graph` for its lifetime, matching RocksGraph's single-process embedded
    model. Multiple graphs means multiple server processes.
  - Auto-generating embeddings server-side. The caller supplies vectors,
    exactly like the rest of RocksGraph's Python API — this project doesn't
    ship an embedding model and shouldn't start via the MCP server either.
  - Remote/networked deployment (a hosted, multi-user MCP server) — out of
    scope; this is a local-first tool for a local-first database.

## Design

### Tool surface

| Tool | Purpose | Maps to |
|------|---------|---------|
| `graph_add_entity` | Add a vertex: `label`, `properties` dict, optional explicit `id`. Returns the assigned id. | `txn.g().addV(label).property(...).next()` |
| `graph_add_relationship` | Add an edge: `src_id`, `dst_id`, `label`, `properties` dict. | `txn.g().addE(label).from_(src).to_(dst).property(...).next()` |
| `graph_search_similar` | Vector search: `property`, `query_vector`, `k`. Returns matching vertices with properties. | `.V([]).nearest(property, query_vector, k).withProperties([])` |
| `graph_get_related` | Bounded neighbor lookup: `entity_id`, `direction` (out/in/both), optional `label` filter, `max_hops` (capped server-side, see below). | `.V([entity_id]).out/in/both([label]).withProperties([])`, repeated up to `max_hops` |
| `graph_get_entity` | Fetch one vertex or edge by id with all properties. | `.V([id]).withProperties([]).next()` |
| `graph_schema` (MCP *resource*, not a tool) | Read-only: declared vertex/edge labels and property key types, so the agent knows what it can query without guessing. | `graph.open_schema()` read path |

`graph_get_related` is the deliberately-constrained substitute for exposing
Gremlin directly — it's a fixed-shape multi-hop neighbor query (label filter +
direction + hop count), not an arbitrary traversal-plan builder. This covers
the actual pattern both Agent Memory (`out("LED_TO")`) and Graph RAG
(`in("NEXT_CHUNK")`/`out("NEXT_CHUNK")`) need, without giving an LLM caller a
general-purpose query language to construct unbounded plans in.

### Constraints / invariants

- **One `Graph` per server process**, opened once at startup from a
  `--db-path` CLI argument. No tool takes a database path — that would imply
  multi-tenancy this design explicitly excludes.
- **`max_hops` is capped server-side** (e.g. a hard ceiling of 5) regardless of
  what the caller requests in `graph_get_related` — an LLM-constructed call is
  an untrusted input with respect to cost, the same way user-supplied
  pagination limits are already treated in the storage layer's own scan APIs.
- **Every mutation tool call is its own committed `TxnSession`** — no
  long-lived transaction spanning multiple tool calls. MCP tool calls are
  independent by design (no session state assumed between them), so holding a
  transaction open across calls would be surprising and leak on a dropped
  connection.
- **`graph_search_similar` requires a declared vector index** on the target
  property. If none exists, `.nearest()` already falls back to an exact
  brute-force scan (see
  [Vector Search Deep Dive](../../guides/vector_search.md#7-vector-search-anti-patterns)) —
  the server should surface this as an explicit warning in the tool response
  rather than silently returning a slow result on every call.

## Planned repo structure

Following the promotion playbook's existing "Integration template" (separate
repo per integration, e.g. `rocksgraph-langchain`, `rocksgraph-ollama`), this
is a new repo, not part of this monorepo:

| Path | Role |
|------|------|
| `rocksgraph-mcp/pyproject.toml` | Depends on `rocksgraph` (PyPI) + the official `mcp` Python SDK |
| `rocksgraph-mcp/server.py` | Tool definitions + JSON-RPC/stdio wiring via the `mcp` SDK |
| `rocksgraph-mcp/README.md` | Claude Desktop / Claude Code config snippet, tool reference |
| `rocksgraph-mcp/tests/` | Unit tests per tool + one stdio protocol-conformance smoke test |

## Implementation plan

1. Write the JSON input/output schema for each tool in the table above — no
   code yet, just the contract, so it can be reviewed before implementation.
2. Scaffold `rocksgraph-mcp` using the official `mcp` Python SDK, wrapping
   `rocksgraph.Graph` opened from `--db-path`.
3. Implement the five tools + the `graph_schema` resource.
4. Write the Claude Desktop / Claude Code config example and a short
   "Getting Started" README.
5. Publish to PyPI, submit to the community MCP servers registry and the
   Awesome-list channels already tracked in the promotion playbook.
6. Cross-link from this repo's `README.md` and from the promotion playbook's
   Phase 3 integrations list.

## Test plan

### Unit tests (per tool)

Against a temp `Graph` instance: each tool call produces the expected
RocksGraph API call and the expected JSON shape back — e.g.
`graph_add_entity` returns the id it created, `graph_search_similar` returns
`[]` (not an error) when the index doesn't exist yet and no vertices carry the
property, `graph_get_related` never returns more than `max_hops` deep even if
asked for more.

### Protocol-conformance smoke test

Spin up the server as a subprocess, send real JSON-RPC tool-call messages over
stdio, assert on the responses — the same "run the real thing end-to-end and
assert on its actual output" approach already used in this repo for
cross-language parity testing (`cross_validate_load`), applied here to
protocol conformance instead of data parity.

### Manual verification

Configure `rocksgraph-mcp` in Claude Desktop against a small test database,
confirm tool discovery works and a full round trip succeeds: add an entity,
search for something similar, fetch its related entities.

## Out of scope

- Multi-graph / multi-tenant serving (§Non-goals)
- Remote/networked MCP transport (§Non-goals)
- Auto-embedding generation (§Non-goals)
- A raw/arbitrary Gremlin query tool — if demand for this shows up post-v1, it
  should be a separate, explicitly opt-in tool (e.g. gated behind
  `--allow-raw-query`), not the default surface.
