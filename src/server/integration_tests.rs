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
    client::gremlin_client::{self, GremlinArgument, GremlinQueryAst, ParsedGremlinStep},
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
async fn _test_server_client_integration() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = "127.0.0.1:8081"; // Use a different port for tests

    // 1. Setup: Create a temporary RocksDB store and populate it with TinkerPop Modern Graph data
    let (graph_store, _dir) = test_utils::open_rocks_store();
    test_utils::create_tinkerpop_modern_graph_for_server_test(Arc::clone(&graph_store));

    // 2. Start the Gremlin server in a background task
    tokio::spawn(async move {
        gremlin_server::start_server(server_addr, graph_store).await.expect("Server failed to start");
    });

    // Give the server a moment to start up
    sleep(Duration::from_millis(100)).await;

    // 3. Connect the Gremlin client
    let mut client = gremlin_client::GremlinClient::connect(server_addr).await?;

    // --- Test Case 1: g.V(1) ---
    println!("\n--- Testing g.V(1) ---");
    let query_v1 = GremlinQueryAst {
        source: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] }],
        step: vec![],
    };
    let response_v1 = client.send_query(query_v1).await?;
    println!("Response for g.V(1): {}", response_v1);
    let data_v1 = test_utils::parse_server_response(&response_v1)?;
    assert_eq!(data_v1.as_array().unwrap().len(), 1); //
    assert_eq!(data_v1[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 2: g.V().count() ---
    println!("\n--- Testing g.V().count() ---");
    let query_v_count = GremlinQueryAst {
        source: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![] }],
        step: vec![ParsedGremlinStep { name: "count".to_string(), arguments: vec![] }],
    };
    let response_v_count = client.send_query(query_v_count).await?;
    println!("Response for g.V().count(): {}", response_v_count);
    let data_v_count = test_utils::parse_server_response(&response_v_count)?;
    assert_eq!(data_v_count.as_array().unwrap().len(), 1); //
    assert_eq!(data_v_count[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int32(6))));

    // --- Test Case 3: g.V().has('name', 'marko') ---
    println!("\n--- Testing g.V().has('name', 'marko') ---");
    let query_has_marko = GremlinQueryAst {
        source: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![] }],
        step: vec![ParsedGremlinStep {
            name: "has".to_string(),
            arguments: vec![GremlinArgument::String("name".to_string()), GremlinArgument::String("marko".to_string())],
        }],
    };
    let response_has_marko = client.send_query(query_has_marko).await?;
    println!("Response for g.V().has('name', 'marko'): {}", response_has_marko);
    let data_has_marko = test_utils::parse_server_response(&response_has_marko)?;
    assert_eq!(data_has_marko.as_array().unwrap().len(), 1); //
    assert_eq!(data_has_marko[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 4: g.V(1).outE('knows').count() ---
    println!("\n--- Testing g.V(1).outE('knows').count() ---");
    let query_marko_knows_count = GremlinQueryAst {
        source: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] }],
        step: vec![
            ParsedGremlinStep {
                name: "outE".to_string(),
                arguments: vec![GremlinArgument::String("knows".to_string())],
            },
            ParsedGremlinStep { name: "count".to_string(), arguments: vec![] },
        ],
    };
    let response_marko_knows_count = client.send_query(query_marko_knows_count).await?;
    println!("Response for g.V(1).outE('knows').count(): {}", response_marko_knows_count);
    let data_marko_knows_count = test_utils::parse_server_response(&response_marko_knows_count)?;
    assert_eq!(data_marko_knows_count.as_array().unwrap().len(), 1); //
    assert_eq!(
        data_marko_knows_count[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int32(2)))
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
