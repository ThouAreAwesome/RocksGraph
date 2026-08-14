# Project Promotion Playbook

Status: actionable checklist
Created: 2026-08-04
Updated: 2026-08-12 — corrected Phase 0/1 status against actual repo state, rescoped Week 2, cross-linked to `docs/ideas/README.md`

> **See also**: [`docs/ideas/README.md`](../ideas/README.md) is the source of truth for demo/tutorial content ideas, each with a Pros/Cons, an effectiveness-measurement plan, and a target audience. Content-plan items below (especially Week 3) should pull from there rather than being invented independently — check it before committing to a tutorial topic.

---

## Phase 0: README Overhaul (before publishing)

> Do this before publishing — crates.io and PyPI show the README as the landing page.
> A user who finds the crate on day 1 should not see a stale first impression.

- [x] Add keywords to both packages (drives search ranking on crates.io and PyPI):
  - `rocksgraph/Cargo.toml` — replace existing 4 keywords with exactly 5 (crates.io limit): `["graph", "gremlin", "embedded", "vector-search", "database"]` (drop `"rocksdb"` — implementation detail, not a search term; add `"embedded"` and `"vector-search"`)
  - `bindings/python/pyproject.toml` — add missing `keywords` field: `["graph-database", "vector-search", "gremlin", "embedded", "rocksdb"]`
- [x] Add badges at top (CI workflow is `ci.yml`, not `tests.yml`):
  - `[![CI](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml/badge.svg)](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml)`
  - `[![crates.io](https://img.shields.io/crates/v/rocksgraph.svg)](https://crates.io/crates/rocksgraph)`
  - `[![docs.rs](https://docs.rs/rocksgraph/badge.svg)](https://docs.rs/rocksgraph)`
  - `[![PyPI](https://img.shields.io/pypi/v/rocksgraph.svg)](https://pypi.org/project/rocksgraph/)`
- [x] Add "30-second quickstart" code block (see template below) — present in `README.md`
- [ ] Add honest maturity statement (see template below) — **verified missing**: grepped `README.md` for "stable/maturity/maintained/production", zero matches. Checked off here but not actually in the README; add before driving any traffic to it.
- [ ] Add versioning contract table — **verified missing**, same check as above. This is the single most important trust signal for a stranger deciding whether to depend on a 0.2.x database; not having it live is a real gap, not a formality.
- [ ] Add maintenance statement: "Actively maintained. Issues responded to within a week. Releases when there's something worth shipping. If I stop, I'll say so here." — **verified missing**.

### Versioning table template

```markdown
| Version | Stability |
|---------|-----------|
| 0.2.x   | API may change. On-disk format may change. Not for production data you can't rebuild. |
| 0.3.x   | (planned) API stable. On-disk format stable. |
| 1.0.0   | (planned) Full backward compatibility for both API and storage. |
```

---

## Phase 1: Ship a Release

> This section was written before v0.2.0 shipped and had gone stale — it referenced v0.2.0 specifically while the repo has since moved through v0.2.1 (tagged but never actually published) to v0.2.2. Rewritten as a repeatable checklist rather than a one-time, version-pinned list.

- [x] Fill `[Unreleased]` in `rocksgraph/CHANGELOG.md` with the release's changes
- [x] Bump version in `rocksgraph/Cargo.toml`, `bindings/python/Cargo.toml` (both the package version and the `rocksgraph` path-dependency spec), and `bindings/python/pyproject.toml` — all four must agree
- [x] Run full test suite: `just full-check && just test` (do not use bare `cargo test --lib` — it skips integration tests and the `--deny warnings` clippy check)
- [x] `git tag v$VERSION && git push --tags` — **already done for v0.2.2** as of this update. Pushing a `v*` tag triggers `python-release.yml` automatically (builds wheels for 5 platforms, runs tests, publishes to PyPI), and `just release` separately handles `cargo publish -p rocksgraph` for crates.io.
- [x] **Verify the release actually completed** — do this before any promotion content goes out:
  - Confirm the `python-release.yml` Actions run for the `v0.2.2` tag finished green (a same-day follow-up commit, `ci: allow manual workflow_dispatch to trigger PyPI release`, suggests the tag-triggered run may not have gone cleanly — worth checking, not assuming)
  - Confirm `rocksgraph` 0.2.2 is actually live on crates.io
  - Confirm `rocksgraph` 0.2.2 is actually live on PyPI
  - Promoting a package that isn't actually installable is the single fastest way to burn the "one first impression" the anti-patterns list below warns about

---

## Phase 2: Content Plan (4 weeks)

### Week 1: "Why I built RocksGraph" blog post

- [ ] Write personal story: what problem you had, why existing tools didn't work, what you built, what you learned
- [ ] NOT a feature list — a narrative
- [ ] Post to: r/rust, Hacker News (Show HN)
- [ ] Coordinate with Twitter/LinkedIn same morning for GitHub trending boost

### Week 2: Benchmark comparison post

- [ ] Title: "RocksGraph vs Kuzu vs SurrealDB: embedded graph(+vector) databases benchmarked" — **Kuzu, not LanceDB**: LanceDB is a pure vector/columnar store, not a graph database, so comparing against it directly undercuts the anti-pattern rule below about comparing to different-use-case tools. Kuzu is an embedded, single-binary graph database without native vector search — a closer, fairer peer, and one where RocksGraph's integrated `.nearest()` is a real differentiator rather than a stretch. Keep SurrealDB as the multi-model comparison.
- [ ] **Reuse existing benchmark infrastructure rather than building a new pipeline**: "1M Wikipedia articles with entity links" requires an entity-linking/NER pipeline this repo doesn't have — that's a separate project, not a week's prep work. Instead, reuse what already exists: the measured numbers in `docs/guides/benchmarks.md`, the SNAP/LDBC bulk-load examples, and the diversified synthetic-LDBC generator (`scripts/generate_synthetic_ldbc.py`, already produces typed properties + `FloatVector` embeddings at a configurable scale)
- [ ] Benchmark: "find articles/entities similar to X, connected to Y within N hops"
- [ ] Compare latency, RAM usage, binary size, setup complexity
- [ ] Be honest where RocksGraph loses. Fair comparisons build trust.

### Week 3: Tutorial + reference repo

- [ ] Title: "How to build local-first RAG with RocksGraph + Ollama" — this is [idea #1, Local-First Graph RAG](../ideas/README.md#1-local-first-graph-rag-retrieval-augmented-generation) in the ideas doc; its effectiveness-measurement section already has the baseline (vector-only retrieval) and metric (answer faithfulness / hallucination rate) to make this tutorial's "why it's better" claim concrete instead of asserted
- [ ] Write step-by-step tutorial
- [ ] Create companion GitHub repo with working code
- [ ] Share on: r/learnrust, r/LocalLLaMA, Ollama Discord

### Week 4: Patch release

- [ ] Fix bugs reported in Weeks 1-3
- [ ] Ship a patch release following the Phase 1 checklist above
- [ ] Post changelog

---

## Phase 3: Integrations (Month 2-3)

### Tier 1 (highest impact)

- [x] **Python bindings**: Already exist. Ensure they're discoverable on PyPI. Prerequisite for everything below.
- [ ] **MCP server** (`rocksgraph-mcp`): design doc at [`design_mcp_server.md`](ingestion-bindings/design_mcp_server.md). Reaches Claude Desktop, Claude Code, Cursor, and Windsurf users with one implementation instead of one integration per framework — arguably higher-leverage than the LangChain/LlamaIndex items below, since it's the direct delivery mechanism for the Agent Memory and Graph RAG ideas rather than a generic vector-store adapter. Not currently on the ideas doc's radar despite being a natural fit for the same "AI/LLM tooling developer" audience.
- [ ] **Ollama connector**: `rocksgraph-ollama` Python package. ~500 lines. Local RAG users, growing fast. Reference repo from Week 3 tutorial.
- [ ] **LangChain integration**: `from rocksgraph import RocksGraphVectorStore`. ~200 lines. Access to every LangChain RAG tutorial. Template: copy the Chroma or Qdrant integration and swap the backend.
- [ ] **LlamaIndex integration**: Same approach. ~200 lines.

### Tier 2

- [ ] **CLI tool**: `rgv query "my query" --graph ./data`. Demo-able, shareable, makes the project tangible. ~300 lines.

### Integration template

For each integration:
1. Create a separate repo: `rocksgraph-langchain`, `rocksgraph-ollama`, etc.
2. Copy an existing integration for the same framework (Chroma, Qdrant, etc.)
3. Swap the backend
4. Add a 10-line README showing how to use it
5. Submit to the framework's integration registry

---

## Phase 4: Maintenance Cadence (Ongoing)

- [ ] **Releases when there's something worth shipping**: Not on a schedule. Every release should have a reason. Regular releases signal project health.
- [ ] **One "vs" blog post per quarter**: Kuzu, SurrealDB, pgvector, Qdrant. Acknowledge where they win. Be the credible source.
- [ ] **Conference talks**: Apply to RustConf, FOSDEM, QCon with problem-solution talks, not product demos. Goal: learn what problems people actually have, not to promote.
- [ ] **This Week in Rust**: Submit to the newsletter monthly. Free, high-signal.

---

## Conversion Funnel

```
Blog post / HN / Reddit
  → Star or bookmark                                (1 in 20 readers)
    → Clone and run quickstart                       (1 in 10 stars)
      → Build internal tool with it                  (1 in 5 clones)
        → Report a bug or feature request            (1 in 3 users)
          → Open-source contributor                  (1 in 10 requesters)
```

Expected numbers for a niche Rust database:

| Time | GitHub Stars | Active Users | Contributors |
|------|-------------|-------------|-------------|
| Month 2 | 50-200 | 10-20 | 0 |
| Month 4 | 200-500 | 30-60 | 1-2 |
| Month 8 | 500-1000 | 50-100 | 3-5 |

---

## Anti-Patterns

- ❌ "It's fast" as the main pitch. Every database says it's fast. Speed is table stakes.
- ❌ Announcing before the benchmark post + LangChain integration are done. One first impression.
- ❌ Building features for imaginary users. Every feature request from a non-user is noise.
- ❌ Comparing to server databases (Neo4j, Redis, Dgraph). They serve a different use case; the comparison reminds readers those tools exist without helping anyone choose. Same logic applies to comparing against pure vector stores (LanceDB, etc.) as if they were graph-database peers.
- ❌ Documentation as an afterthought. README is read 1000× more than code.
- ❌ Inventing tutorial/demo topics ad hoc instead of pulling from `docs/ideas/README.md` — that doc already has the Pros/Cons and effectiveness-measurement work done; reinventing it here risks picking a topic whose "why it's better" claim was never actually validated.
- ❌ Treating a checklist item as done because it's checked off. Verify against the actual README/repo state before relying on it — this playbook itself had three Phase 0 items marked `[x]` that were never actually added.

---

## Target Channels

| Channel | Why | When |
|---------|-----|------|
| r/rust | Users write Rust. Post benchmark results, not announcements. | Week 1 |
| Hacker News (Show HN) | One shot. Coordinate with Reddit + Twitter same morning. | Week 1 |
| This Week in Rust | Free newsletter, high-signal. Submit monthly. | Ongoing |
| LangChain Discord | People building RAG pipelines right now. Offer to help. | Month 2 |
| Ollama Discord | Local RAG users. Reference docs in answers. | Month 2 |
| r/learnrust, r/LocalLLaMA | Named in Week 3 as share targets for the RAG tutorial — added here for consistency. | Week 3 |
| Awesome-Rust / Awesome-Embedded-Database lists | One-time PR, near-zero effort, compounding organic discovery for as long as the list exists. Currently not on the plan at all. | Month 2 |
| RustConf/FOSDEM | Learn user problems, not promote. | 2027 |
| GitHub trending | Requires ~80 stars in a day. Coordinate launch. | Week 1 |
| crates.io top downloads | Organic. No action needed. | Ongoing |

---

## Success Metric

> "Will the 50 people who need this desperately find it when they search 'embedded graph vector database Rust'?"

If yes, the project succeeded. 50 passionate users > 5000 star-collectors.
