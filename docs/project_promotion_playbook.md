# Project Promotion Playbook

Status: actionable checklist
Created: 2026-08-04

---

## Phase 0: Ship v0.2 (EOW)

- [ ] Bump version to 0.2.0 in Cargo.toml
- [ ] Run full test suite: `cargo test --lib && cargo test --doc && cargo fmt --check && cargo clippy --lib`
- [ ] `cargo publish`
- [ ] Tag release on GitHub: `git tag v0.2.0 && git push origin v0.2.0`

---

## Phase 1: README Overhaul (EOW)

- [ ] Add badges at top:
  - `[![tests](.../actions/workflows/tests.yml/badge.svg)]`
  - `[![crates.io](https://img.shields.io/crates/v/rocksgraph.svg)]`
  - `[![docs.rs](https://docs.rs/rocksgraph/badge.svg)]`
- [ ] Add honest maturity statement (see template below)
- [ ] Add "5 minute quickstart" code block (see template below)
- [ ] Add versioning contract table
- [ ] Add maintenance statement: "Actively maintained. Monthly releases. If I stop, I'll say so here."

### README maturity statement template

```markdown
## RocksGraph

An **embedded** graph + vector database for Rust. Early stage, production-curious.

**What's solid:**
- HNSW vector search (810+ tests, WAL crash recovery)
- ACID transactions (OCC, RYOW, rollback)
- Property graph model (vertices, edges, properties, labels)
- Gremlin-like traversal engine
- ~800 KB binary overhead

**What's not:**
- No distributed/cluster mode (and won't have one)
- No SQL/GQL query language (use the Rust API)
- Not yet fuzzed or Jepsen-tested
- Snapshot persistence is WAL-based, not incremental

**Who should use it:**
- You're building a local-first Rust application that needs graph + vector queries
- You're running RAG on edge devices or embedded systems
- You want ACID without running a database server

**Who shouldn't:**
- You need horizontal scaling (use Neo4j or Dgraph)
- You need sub-millisecond single-key lookup (use SQLite)
```

### Quickstart template

```rust
use rocksgraph::Graph;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let graph = Graph::open("./my_db")?;
    let mut txn = graph.begin()?;

    let alice = txn.add_vertex("person", [("name", "Alice"), ("age", 30)])?;
    let bob = txn.add_vertex("person", [("name", "Bob"), ("age", 28)])?;
    txn.add_edge(alice, bob, "knows", [("since", 2020)])?;
    txn.commit()?;

    Ok(())
}
```

### Versioning table template

```markdown
| Version | Stability |
|---------|-----------|
| 0.2.x   | API may change. On-disk format may change. Not for production data you can't rebuild. |
| 0.4.x   | (planned) API stable. On-disk format stable. |
| 1.0.0   | (planned) Full backward compatibility for both API and storage. |
```

---

## Phase 2: Content Plan (4 weeks)

### Week 1: "Why I built RocksGraph" blog post

- [ ] Write personal story: what problem you had, why existing tools didn't work, what you built, what you learned
- [ ] NOT a feature list — a narrative
- [ ] Post to: r/rust, Hacker News (Show HN)
- [ ] Coordinate with Twitter/LinkedIn same morning for GitHub trending boost

### Week 2: Benchmark comparison post

- [ ] Title: "RocksGraph vs SurrealDB vs Neo4j: embedded graph databases benchmarked"
- [ ] Load 1M Wikipedia articles with entity links
- [ ] Benchmark: "find articles similar to X, authored by people in Y's network"
- [ ] Compare latency, RAM usage, binary size, setup complexity
- [ ] Be honest where RocksGraph loses. Fair comparisons build trust.

### Week 3: Tutorial + reference repo

- [ ] Title: "How to build local-first RAG with RocksGraph + Ollama"
- [ ] Write step-by-step tutorial
- [ ] Create companion GitHub repo with working code
- [ ] Share on: r/learnrust, r/LocalLLaMA, Ollama Discord

### Week 4: v0.2.1 release

- [ ] Fix bugs reported in Weeks 1-3
- [ ] Ship patch release
- [ ] Post changelog

---

## Phase 3: Integrations (Month 2-3)

### Tier 1 (highest impact)

- [ ] **Ollama plugin**: ~500 lines. Local RAG users, growing fast. Reference repo from Week 3 tutorial.
- [ ] **LangChain integration**: `from rocksgraph import RocksGraphVectorStore`. ~200 lines. Access to every LangChain RAG tutorial. Template: copy the Chroma or Qdrant integration and swap the backend.
- [ ] **LlamaIndex integration**: Same approach. ~200 lines.

### Tier 2

- [ ] **Python bindings**: Already exist. Ensure they're discoverable with separate crates.io + PyPI package.
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

- [ ] **Monthly releases**: Ship something every 4 weeks. Even a patch release with one bugfix. Regular releases signal project health.
- [ ] **One "vs" blog post per quarter**: Neo4j, SurrealDB, pgvector, Qdrant, LanceDB. Acknowledge where they win. Be the credible source.
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
- ❌ Comparing to Redis. Every comparison to a well-known tool reminds people that tool exists.
- ❌ Documentation as an afterthought. README is read 1000× more than code.

---

## Target Channels

| Channel | Why | When |
|---------|-----|------|
| r/rust | Users write Rust. Post benchmark results, not announcements. | Week 1 |
| Hacker News (Show HN) | One shot. Coordinate with Reddit + Twitter same morning. | Week 1 |
| This Week in Rust | Free newsletter, high-signal. Submit monthly. | Ongoing |
| LangChain Discord | People building RAG pipelines right now. Offer to help. | Month 2 |
| Ollama Discord | Local RAG users. Reference docs in answers. | Month 2 |
| RustConf/FOSDEM | Learn user problems, not promote. | 2027 |
| GitHub trending | Requires ~80 stars in a day. Coordinate launch. | Week 1 |
| crates.io top downloads | Organic. No action needed. | Ongoing |

---

## Success Metric

> "Will the 50 people who need this desperately find it when they search 'embedded graph vector database Rust'?"

If yes, the project succeeded. 50 passionate users > 5000 star-collectors.
