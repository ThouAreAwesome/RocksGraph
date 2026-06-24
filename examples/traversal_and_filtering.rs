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

//! Demonstrates the read-side traversal vocabulary against the classic TinkerPop
//! "modern" toy graph (the same dataset used throughout the test suite):
//! 1. Traversal direction steps: `out`/`r#in`/`both`, `outE`/`inE`/`bothE`, `inV`/`outV`/`otherV`.
//! 2. Filtering: `has`/`hasLabel`/`hasId`/`is`/`where`/`limit`/`dedup`. `has()`/`is()` accept any
//!    `Predicate` (`eq`/`ne`/`gt`/`gte`/`lt`/`lte`/`between`/`within`/`without`) on user
//!    properties and on `Key::Id`; `Key::Label` supports all but the range predicates
//!    (`gt`/`gte`/`lt`/`lte`/`between`), since labels have no ordering.
//! 3. Extraction: `values`/`properties`/`count`/`fold`/`path`.
//! 4. The lazy `iter()` terminal, as an alternative to `next()`/`to_list()`.

use rocksgraph::{gt, Graph, TraversalBuilder, Value, __};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let graph = Graph::open(temp_dir.path())?;

    // --- Setup: the TinkerPop "modern" graph ---
    // person: marko(29), vadas(27), josh(32), peter(35)
    // software: lop(java), ripple(java)
    // marko -knows-> vadas (weight 0.5), marko -knows-> josh (weight 1.0)
    // marko -created-> lop (0.4), josh -created-> ripple (1.0), josh -created-> lop (0.4)
    // peter -created-> lop (0.2)
    let mut tx = graph.begin();
    tx.g().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32).next()?;
    tx.g().addV("person").property("id", 2i64).property("name", "vadas").property("age", 27i32).next()?;
    tx.g().addV("software").property("id", 3i64).property("name", "lop").property("lang", "java").next()?;
    tx.g().addV("person").property("id", 4i64).property("name", "josh").property("age", 32i32).next()?;
    tx.g().addV("software").property("id", 5i64).property("name", "ripple").property("lang", "java").next()?;
    tx.g().addV("person").property("id", 6i64).property("name", "peter").property("age", 35i32).next()?;
    tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).next()?;
    tx.g().addE("knows").from(1).to(4).property("weight", 1.0f64).next()?;
    tx.g().addE("created").from(1).to(3).property("weight", 0.4f64).next()?;
    tx.g().addE("created").from(4).to(5).property("weight", 1.0f64).next()?;
    tx.g().addE("created").from(4).to(3).property("weight", 0.4f64).next()?;
    tx.g().addE("created").from(6).to(3).property("weight", 0.2f64).next()?;
    tx.commit()?;

    let mut g = graph.read();

    // --- Traversal direction steps ---
    println!("=== Traversal directions ===");
    let marko_friends = g.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
    println!("marko -out(knows)-> {:?}", marko_friends);

    let lop_creators = g.g().V([3]).r#in(["created"]).values(["name"]).to_list()?;
    println!("lop <-in(created)- {:?}", lop_creators);

    let josh_neighbors = g.g().V([4]).both(["knows", "created"]).values(["name"]).to_list()?;
    println!("josh -both(knows,created)- {:?}", josh_neighbors);

    let marko_out_edges = g.g().V([1]).outE(["knows"]).count().next()?;
    println!("marko outE(knows) count = {:?}", marko_out_edges);

    // inV()/outV()/otherV() pivot from an edge traverser to one of its endpoints.
    let knows_targets = g.g().V([1]).outE(["knows"]).inV().values(["name"]).to_list()?;
    println!("marko outE(knows).inV() = {:?}", knows_targets);
    let knows_sources = g.g().V([2]).inE(["knows"]).outV().values(["name"]).to_list()?;
    println!("vadas inE(knows).outV() = {:?}", knows_sources);
    let other_side = g.g().V([1]).bothE(["knows"]).otherV().values(["name"]).to_list()?;
    println!("marko bothE(knows).otherV() = {:?}", other_side);

    // --- Filtering ---
    println!("\n=== Filtering ===");
    // `has(key, value)` with a plain scalar is shorthand for `Predicate::Eq`.
    let josh_by_age = g.g().V([]).hasLabel(["person"]).has("age", 32i32).values(["name"]).to_list()?;
    println!("person.age == 32: {:?}", josh_by_age);

    let by_name = g.g().V([]).has("name", "peter").values(["age"]).to_list()?;
    println!("name == peter, age: {:?}", by_name);

    // Range predicates work on properties too, not just Eq.
    let over_30 = g.g().V([]).hasLabel(["person"]).has("age", gt(30i32)).values(["name"]).to_list()?;
    println!("person.age > 30: {:?}", over_30);

    // `hasId` accepts multiple ids — backed by `Predicate::Within`.
    let marko_and_josh = g.g().V([]).hasId([1, 4]).hasLabel(["person"]).values(["name"]).to_list()?;
    println!("hasId([1, 4]).hasLabel(person): {:?}", marko_and_josh);

    // `where()` filters the current traverser using a side traversal (here: people who created
    // at least one piece of software).
    let creators = g.g().V([]).hasLabel(["person"]).r#where(__().out(["created"])).values(["name"]).to_list()?;
    println!("people who created something: {:?}", creators);

    // `is()` filters a scalar value already on the traverser (post-values()).
    let exactly_29 = g.g().V([]).hasLabel(["person"]).values(["age"]).is(29i32).to_list()?;
    println!("age is exactly 29: {:?}", exactly_29);

    let first_two = g.g().V([]).hasLabel(["person"]).limit(2).values(["name"]).to_list()?;
    println!("limit(2): {:?}", first_two);

    let unique_langs = g.g().V([]).hasLabel(["software"]).values(["lang"]).dedup().to_list()?;
    println!("dedup(software.lang): {:?}", unique_langs);

    // --- Extraction & aggregation ---
    println!("\n=== Extraction & aggregation ===");
    let lop_edge_weights = g.g().V([3]).inE(["created"]).values(["weight"]).fold().next()?;
    println!("lop inE(created).weight, folded: {:?}", lop_edge_weights);

    let lop_props = g.g().V([3]).properties(["name", "lang"]).to_list()?;
    println!("lop.properties([name, lang]): {:?}", lop_props);

    let path = g.g().V([1]).out(["knows"]).out(["created"]).path().next()?;
    println!("marko -> knows -> created, path(): {:?}", path);

    // --- The lazy `iter()` terminal ---
    println!("\n=== Lazy iteration ===");
    for result in g.g().V([]).hasLabel(["person"]).values(["name"]).iter()? {
        match result? {
            Value::String(name) => println!("person (lazy): {}", name),
            other => println!("unexpected value: {:?}", other),
        }
    }

    Ok(())
}
