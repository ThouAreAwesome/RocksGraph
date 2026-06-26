// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

//! Demonstrates `explain()` — the physical plan pretty-printer.
//!
//! Builds a small graph, then prints the physical plans of several
//! traversals, showing how the optimizer chooses operators like
//! VStep index seeks, GetEStep point lookups, and branching structures.

use rocksgraph::{Graph, TraversalBuilder, __};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let graph = Graph::open(temp_dir.path())?;

    // Seed some data.
    let mut tx = graph.begin();
    tx.g().addV("person").property("id", 1i64).property("name", "alice").property("age", 30i32).next()?;
    tx.g().addV("person").property("id", 2i64).property("name", "bob").property("age", 25i32).next()?;
    tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).next()?;
    tx.commit()?;

    let mut snap = graph.read();

    // ── Basic linear plan ────────────────────────────────────────────────
    println!("=== g.V([1]).out(\"knows\").count() ===");
    let plan = snap.g().V([1]).out(["knows"]).count().explain()?;
    println!("{}", plan);

    // ── V + hasId folding into an index seek ─────────────────────────────
    println!("=== g.V([]).hasId([1]).values(\"name\") ===");
    let plan = snap.g().V([]).hasId([1]).values(["name"]).explain()?;
    println!("{}", plan);

    // ── outE + where(otherV.hasId) folded into GetEStep point lookup ─────
    println!("=== g.V([1]).outE(\"knows\").where(otherV().hasId([2])) ===");
    let plan = snap.g().V([1]).outE(["knows"]).r#where(__().otherV().hasId([2])).explain()?;
    println!("{}", plan);

    // ── Branching: union ─────────────────────────────────────────────────
    println!("=== g.V([1]).union(outE(\"knows\"), inE(\"knows\")) ===");
    let plan = snap.g().V([1]).union([__().outE(["knows"]), __().inE(["knows"])]).explain()?;
    println!("{}", plan);

    // ── Multi-hop with hasLabel ──────────────────────────────────────────
    println!("=== g.V([1]).out(\"knows\").both(\"knows\").hasLabel(\"person\").count() ===");
    let plan = snap.g().V([1]).out(["knows"]).both(["knows"]).hasLabel(["person"]).count().explain()?;
    println!("{}", plan);

    // ── Repeat loop ──────────────────────────────────────────────────────
    println!("=== g.V([1]).repeat(out(\"knows\")).times(3) ===");
    let plan = snap.g().V([1]).repeat(__().out(["knows"])).times(3).explain()?;
    println!("{}", plan);

    // ── Repeat + until ───────────────────────────────────────────────────
    println!("=== g.V([1]).repeat(out(\"knows\")).until(hasId([2])) ===");
    let plan = snap.g().V([1]).repeat(__().out(["knows"])).until(__().hasId([2])).explain()?;
    println!("{}", plan);

    Ok(())
}
