// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use tokio::time::{sleep, Duration};

use crate::{
    client::gremlin_client::{self, GremlinArgument},
    server::{gremlin_server, test_utils},
    types::gvalue::Primitive,
};

/// Binds an OS-assigned port and returns its address string.
/// Prevents hard-coded port collisions when tests run in parallel.
async fn random_server_addr() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

#[tokio::test]
async fn test_server_client_integration() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = random_server_addr().await;
    let server_addr_clone = server_addr.clone();
    // 1. Setup: Create a temporary RocksDB store and populate it with TinkerPop Modern Graph data
    let (graph_store, _dir) = test_utils::open_rocks_store();
    test_utils::create_tinkerpop_modern_graph_for_server_test(Arc::clone(&graph_store));

    // 2. Start the Gremlin server in a background task
    tokio::spawn(async move {
        gremlin_server::start_server(&server_addr.clone(), graph_store).await.expect("Server failed to start");
    });

    // Give the server a moment to start up
    sleep(Duration::from_millis(100)).await;

    // 3. Connect the Gremlin client
    let mut client = gremlin_client::GremlinClient::connect(&server_addr_clone).await?;
    let mut g = gremlin_client::graphTraversalSource(&mut client);

    // --- Test Case 1: g.V(1) ---
    println!("\n--- Testing g.V(1) ---");
    let data_v1 = g.V(&[1]).execute().await?;
    println!("Response for g.V(1): {}", data_v1);
    assert_eq!(data_v1.as_array().unwrap().len(), 1); //
    assert_eq!(data_v1[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 2: g.V().count() ---
    // DISABLED: Full table scan is not yet supported.
    /*
    println!("\n--- Testing g.V().count() ---");
    let query_v_count = GremlinQueryAst {
        source: vec![],
        step: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![] }, ParsedGremlinStep { name: "count".to_string(), arguments: vec![] }],
    };
    let response_v_count = client.send_query(query_v_count).await?;
    println!("Response for g.V().count(): {}", response_v_count);
    let data_v_count = test_utils::parse_server_response(&response_v_count)?;
    assert_eq!(data_v_count.as_array().unwrap().len(), 1); //
    assert_eq!(data_v_count[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int32(6))));
    */

    // --- Test Case 3: g.V(1).has('name', 'marko') ---
    println!("\n--- Testing g.V(1).has('name', 'marko') ---");
    let data_has_marko = g.reset().V(&[1]).has("name", GremlinArgument::String("marko".to_string())).execute().await?;
    println!("Response for g.V(1).has('name', 'marko'): {}", data_has_marko);
    assert_eq!(data_has_marko.as_array().unwrap().len(), 1); //
    assert_eq!(data_has_marko[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 4: g.V(1).outE('knows').count() ---
    println!("\n--- Testing g.V(1).outE('knows').count() ---");
    let data_marko_knows_count = g.reset().V(&[1]).outE(&[3]).count().execute().await?;
    println!("Response for g.V(1).outE('knows').count(): {}", data_marko_knows_count);
    assert_eq!(data_marko_knows_count.as_array().unwrap().len(), 1); //
    assert_eq!(
        data_marko_knows_count[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int64(2)))
    );

    // --- Test Case 5: g.V(1).out() ---
    println!("\n--- Testing g.V(1).out() ---");
    let data_out = g.reset().V(&[1]).out(&[]).execute().await?;
    assert_eq!(data_out.as_array().unwrap().len(), 3);

    // --- Test Case 6: g.V(4).bothE() ---
    println!("\n--- Testing g.V(4).bothE() ---");
    let data_bothe = g.reset().V(&[4]).bothE(&[]).execute().await?;
    assert_eq!(data_bothe.as_array().unwrap().len(), 3);

    // --- Test Case 7: g.V(1).out().hasLabel('software') ---
    println!("\n--- Testing g.V(1).out().hasLabel('software') ---");
    let data_has_label = g.reset().V(&[1]).out(&[]).hasLabel(&[2]).execute().await?;
    assert_eq!(data_has_label.as_array().unwrap().len(), 1);
    assert_eq!(data_has_label[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(3)));

    // --- Test Case 8: g.V(1).values('name') ---
    println!("\n--- Testing g.V(1).values('name') ---");
    let data_values = g.reset().V(&[1]).values(&["name"]).execute().await?;
    assert_eq!(data_values.as_array().unwrap().len(), 1);
    assert_eq!(
        data_values[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "marko"
        ))))
    );

    // --- Test Case 9: g.V(1).values('age').is(29) ---
    println!("\n--- Testing g.V(1).values('age').is(29) ---");
    let data_is = g.reset().V(&[1]).values(&["age"]).is(GremlinArgument::Int(29)).execute().await?;
    assert_eq!(data_is.as_array().unwrap().len(), 1);
    assert_eq!(data_is[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int64(29))));

    // --- Test Case 10: g.V(1).where(out().hasLabel('software')) ---
    println!("\n--- Testing g.V(1).where(out().hasLabel('software')) ---");
    let mut sub_query = gremlin_client::__();
    sub_query.out(&[]).hasLabel(&[2]);
    let data_where = g.reset().V(&[1]).r#where(&mut sub_query).execute().await?;
    assert_eq!(data_where.as_array().unwrap().len(), 1);
    assert_eq!(data_where[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 11: g.V(1).out().hasLabel('person', 'software') ---
    println!("\n--- Testing g.V(1).out().hasLabel('person', 'software') ---");
    let data_has_labels = g.reset().V(&[1]).out(&[]).hasLabel(&[1, 2]).execute().await?;
    assert_eq!(data_has_labels.as_array().unwrap().len(), 3);

    // --- Test Case 12: g.V(1).out('knows', 'created') ---
    println!("\n--- Testing g.V(1).out('knows', 'created') ---");
    let data_out_multiple = g.reset().V(&[1]).out(&[3, 4]).execute().await?;
    // Marko has 2 'knows' and 1 'created' outward edges
    assert_eq!(data_out_multiple.as_array().unwrap().len(), 3);

    // --- Test Case 13: g.V(1).outE() ---
    println!("\n--- Testing g.V(1).outE() ---");
    let data_oute = g.reset().V(&[1]).outE(&[]).execute().await?;
    assert_eq!(data_oute.as_array().unwrap().len(), 3);

    // --- Test Case 14: g.V(3).inE() ---
    println!("\n--- Testing g.V(3).inE() ---");
    let data_ine = g.reset().V(&[3]).inE(&[]).execute().await?;
    assert_eq!(data_ine.as_array().unwrap().len(), 3);

    // --- Test Case 15: g.V(3).in() ---
    println!("\n--- Testing g.V(3).in() ---");
    let data_in = g.reset().V(&[3]).r#in(&[]).execute().await?;
    assert_eq!(data_in.as_array().unwrap().len(), 3);

    // --- Test Case 16: g.V(4).both() ---
    println!("\n--- Testing g.V(4).both() ---");
    let data_both = g.reset().V(&[4]).both(&[]).execute().await?;
    assert_eq!(data_both.as_array().unwrap().len(), 3);

    // --- Test Case 17: g.V(1).out().limit(2) ---
    println!("\n--- Testing g.V(1).out().limit(2) ---");
    let data_limit = g.reset().V(&[1]).out(&[]).limit(2).execute().await?;
    assert_eq!(data_limit.as_array().unwrap().len(), 2);

    // --- Test Case 18: g.V().hasId(1, 2) ---
    println!("\n--- Testing g.V(1, 2, 3).hasId(1, 2) ---");
    let data_has_id = g.reset().V(&[1, 2, 3]).hasId(&[1, 2]).execute().await?;
    assert_eq!(data_has_id.as_array().unwrap().len(), 2);

    // --- Test Case 19: g.V(4).both().limit(2) ---
    println!("\n--- Testing g.V(4).both().limit(2) ---");
    let data_both = g.reset().V(&[4]).both(&[]).limit(2).execute().await?;
    assert_eq!(data_both.as_array().unwrap().len(), 2);

    // --- Test Case 20: g.V(1).hasId(1).outE().where(otherV().hasId(2, 3)).count() ---
    println!("\n--- Testing g.V(1).hasId(1).outE().where(otherV().hasId(2, 3)).count() ---");
    let mut sub_query_other = gremlin_client::__();
    sub_query_other.otherV().hasId(&[2, 3]);
    let data_complex =
        g.reset().V(&[1, 2, 3]).hasId(&[1]).outE(&[]).r#where(&mut sub_query_other).count().execute().await?;
    assert_eq!(data_complex.as_array().unwrap().len(), 1);
    assert_eq!(data_complex[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int64(2))));

    Ok(())
}

#[tokio::test]
async fn test_sequential_modifications() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = random_server_addr().await;

    // 1. Setup: Create a temporary, empty RocksDB store
    let (graph_store, _dir) = test_utils::open_rocks_store();

    // 2. Start the Gremlin server in a background task
    let addr_clone = server_addr.clone();
    tokio::spawn(async move {
        gremlin_server::start_server(&addr_clone, graph_store).await.expect("Server failed to start");
    });

    sleep(Duration::from_millis(100)).await;

    // 3. Connect the Gremlin client
    let mut client = gremlin_client::GremlinClient::connect(&server_addr).await?;
    let mut g = gremlin_client::graphTraversalSource(&mut client);

    // --- Step 1: Create vertices ---
    println!("\n--- Testing AddV ---");
    g.reset()
        .addV(
            1,
            101,
            std::collections::HashMap::from([("name".to_string(), GremlinArgument::String("Alice".to_string()))]),
        )
        .execute()
        .await?;

    g.reset()
        .addV(
            1,
            102,
            std::collections::HashMap::from([("name".to_string(), GremlinArgument::String("Bob".to_string()))]),
        )
        .execute()
        .await?;

    // --- Verification 1: Check vertices exist ---
    println!("\n--- Verifying AddV ---");
    let data_alice = g.reset().V(&[101]).values(&["name"]).execute().await?;
    assert_eq!(data_alice.as_array().unwrap().len(), 1);
    assert_eq!(
        data_alice[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "Alice"
        ))))
    );

    // --- Step 2: Create an edge ---
    println!("\n--- Testing AddE ---");
    g.reset()
        .addE(3, 101, 102, std::collections::HashMap::from([("since".to_string(), GremlinArgument::Int(2020))]))
        .execute()
        .await?;

    // --- Verification 2: Check edge exists ---
    println!("\n--- Verifying AddE ---");
    let data_edge = g.reset().V(&[101]).out(&[3]).execute().await?;
    assert_eq!(data_edge.as_array().unwrap().len(), 1);
    assert_eq!(data_edge[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(102)));

    // --- Step 3: Modify vertex property ---
    println!("\n--- Testing Vertex Property Update ---");
    g.reset().V(&[101]).property("name", GremlinArgument::String("Alicia".to_string())).execute().await?;

    // --- Verification 3: Check vertex property update ---
    println!("\n--- Verifying Vertex Property Update ---");
    let data_update = g.reset().V(&[101]).values(&["name"]).execute().await?;
    assert_eq!(
        data_update[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "Alicia"
        ))))
    );

    // --- Step 4: Modify edge property ---
    println!("\n--- Testing Edge Property Update ---");
    g.reset().V(&[101]).outE(&[3]).property("since", GremlinArgument::Int(2022)).execute().await?;

    // --- Verification 4: Check edge property update ---
    println!("\n--- Verifying Edge Property Update ---");
    let data_edge_update = g.reset().V(&[101]).outE(&[3]).values(&["since"]).execute().await?;
    assert_eq!(
        data_edge_update[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int64(2022)))
    );

    Ok(())
}

/// Verifies WebSocket establishment by exercising the Ping/Pong control frame
/// exchange, with no Gremlin query logic involved.
///
/// The server already handles `Message::Ping` by echoing the payload back as
/// `Message::Pong` (see `gremlin_server::handle_connection`). The client's
/// `send_ping` sends the frame and blocks until the matching Pong arrives.
///
/// Three cases are covered:
///  1. Empty payload  — baseline for zero-length echo.
///  2. Non-empty payload — byte-for-byte echo fidelity.
///  3. Five consecutive pings — connection stays healthy across multiple rounds.
#[tokio::test]
async fn test_websocket_ping_pong() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = random_server_addr().await;

    // An empty store is sufficient; ping-pong never touches the graph.
    let (graph_store, _dir) = test_utils::open_rocks_store();
    let addr_clone = server_addr.clone();
    tokio::spawn(async move {
        gremlin_server::start_server(&addr_clone, graph_store).await.expect("Server failed to start");
    });
    sleep(Duration::from_millis(100)).await;

    let mut client = gremlin_client::GremlinClient::connect(&server_addr).await?;

    // Case 1: empty payload — server must echo an empty pong.
    let pong = client.send_ping(vec![]).await?;
    assert_eq!(pong, Vec::<u8>::new(), "empty ping should produce empty pong");

    // Case 2: non-empty payload — server must echo the exact bytes.
    let payload = b"multigraph-ping".to_vec();
    let pong = client.send_ping(payload.clone()).await?;
    assert_eq!(pong, payload, "pong payload must match ping payload");

    // Case 3: five consecutive pings — connection must remain healthy.
    for i in 0u8..5 {
        let p = vec![i, i.wrapping_add(1), i.wrapping_add(2)];
        let pong = client.send_ping(p.clone()).await?;
        assert_eq!(pong, p, "pong #{i} payload mismatch");
    }

    Ok(())
}
