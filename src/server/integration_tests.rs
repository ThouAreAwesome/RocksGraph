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
    // --- Test Case 1: g.V(1) ---
    println!("\n--- Testing g.V(1) ---");
    let query_v1 = GremlinQueryAst {
        source: vec![],
        step: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] }],
    };
    let response_v1 = client.send_query(query_v1).await?;
    println!("Response for g.V(1): {}", response_v1);
    let data_v1 = test_utils::parse_server_response(&response_v1)?;
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
    let query_has_marko = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep {
                name: "has".to_string(),
                arguments: vec![
                    GremlinArgument::String("name".to_string()),
                    GremlinArgument::String("marko".to_string()),
                ],
            },
        ],
    };
    let response_has_marko = client.send_query(query_has_marko).await?;
    println!("Response for g.V(1).has('name', 'marko'): {}", response_has_marko);
    let data_has_marko = test_utils::parse_server_response(&response_has_marko)?;
    assert_eq!(data_has_marko.as_array().unwrap().len(), 1); //
    assert_eq!(data_has_marko[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 4: g.V(1).outE('knows').count() ---
    println!("\n--- Testing g.V(1).outE('knows').count() ---");
    let query_marko_knows_count = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
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

    // --- Test Case 5: g.V(1).out() ---
    println!("\n--- Testing g.V(1).out() ---");
    let query_out = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep { name: "out".to_string(), arguments: vec![] },
        ],
    };
    let response_out = client.send_query(query_out).await?;
    let data_out = test_utils::parse_server_response(&response_out)?;
    assert_eq!(data_out.as_array().unwrap().len(), 3);

    // --- Test Case 6: g.V(4).bothE() ---
    println!("\n--- Testing g.V(4).bothE() ---");
    let query_bothe = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(4)] },
            ParsedGremlinStep { name: "bothE".to_string(), arguments: vec![] },
        ],
    };
    let response_bothe = client.send_query(query_bothe).await?;
    let data_bothe = test_utils::parse_server_response(&response_bothe)?;
    assert_eq!(data_bothe.as_array().unwrap().len(), 3);

    // --- Test Case 7: g.V(1).out().hasLabel('software') ---
    println!("\n--- Testing g.V(1).out().hasLabel('software') ---");
    let query_has_label = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep { name: "out".to_string(), arguments: vec![] },
            ParsedGremlinStep {
                name: "hasLabel".to_string(),
                arguments: vec![GremlinArgument::String("software".to_string())],
            },
        ],
    };
    let response_has_label = client.send_query(query_has_label).await?;
    let data_has_label = test_utils::parse_server_response(&response_has_label)?;
    assert_eq!(data_has_label.as_array().unwrap().len(), 1);
    assert_eq!(data_has_label[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(3)));

    // --- Test Case 8: g.V(1).values('name') ---
    println!("\n--- Testing g.V(1).values('name') ---");
    let query_values = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep {
                name: "values".to_string(),
                arguments: vec![GremlinArgument::String("name".to_string())],
            },
        ],
    };
    let response_values = client.send_query(query_values).await?;
    let data_values = test_utils::parse_server_response(&response_values)?;
    assert_eq!(data_values.as_array().unwrap().len(), 1);
    assert_eq!(
        data_values[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "marko"
        ))))
    );

    // --- Test Case 9: g.V(1).values('age').is(29) ---
    println!("\n--- Testing g.V(1).values('age').is(29) ---");
    let query_is = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep {
                name: "values".to_string(),
                arguments: vec![GremlinArgument::String("age".to_string())],
            },
            ParsedGremlinStep { name: "is".to_string(), arguments: vec![GremlinArgument::Int(29)] },
        ],
    };
    let response_is = client.send_query(query_is).await?;
    let data_is = test_utils::parse_server_response(&response_is)?;
    assert_eq!(data_is.as_array().unwrap().len(), 1);
    assert_eq!(data_is[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int32(29))));

    // --- Test Case 10: g.V(1).where(out().hasLabel('software')) ---
    println!("\n--- Testing g.V(1).where(out().hasLabel('software')) ---");
    let sub_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "out".to_string(), arguments: vec![] },
            ParsedGremlinStep {
                name: "hasLabel".to_string(),
                arguments: vec![GremlinArgument::String("software".to_string())],
            },
        ],
    };
    let query_where = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep {
                name: "where".to_string(),
                arguments: vec![GremlinArgument::NestedBytecode(sub_query)],
            },
        ],
    };
    let response_where = client.send_query(query_where).await?;
    let data_where = test_utils::parse_server_response(&response_where)?;
    assert_eq!(data_where.as_array().unwrap().len(), 1);
    assert_eq!(data_where[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(1)));

    // --- Test Case 11: g.V(1).out().hasLabel('person', 'software') ---
    println!("\n--- Testing g.V(1).out().hasLabel('person', 'software') ---");
    let query_has_labels = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep { name: "out".to_string(), arguments: vec![] },
            ParsedGremlinStep {
                name: "hasLabel".to_string(),
                arguments: vec![
                    GremlinArgument::String("person".to_string()),
                    GremlinArgument::String("software".to_string()),
                ],
            },
        ],
    };
    let response_has_labels = client.send_query(query_has_labels).await?;
    let data_has_labels = test_utils::parse_server_response(&response_has_labels)?;
    assert_eq!(data_has_labels.as_array().unwrap().len(), 3);

    // --- Test Case 12: g.V(1).out('knows', 'created') ---
    println!("\n--- Testing g.V(1).out('knows', 'created') ---");
    let query_out_multiple = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep {
                name: "out".to_string(),
                arguments: vec![
                    GremlinArgument::String("knows".to_string()),
                    GremlinArgument::String("created".to_string()),
                ],
            },
        ],
    };
    let response_out_multiple = client.send_query(query_out_multiple).await?;
    let data_out_multiple = test_utils::parse_server_response(&response_out_multiple)?;
    // Marko has 2 'knows' and 1 'created' outward edges
    assert_eq!(data_out_multiple.as_array().unwrap().len(), 3);

    // --- Test Case 13: g.V(1).outE() ---
    println!("\n--- Testing g.V(1).outE() ---");
    let query_oute = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(1)] },
            ParsedGremlinStep { name: "outE".to_string(), arguments: vec![] },
        ],
    };
    let response_oute = client.send_query(query_oute).await?;
    let data_oute = test_utils::parse_server_response(&response_oute)?;
    assert_eq!(data_oute.as_array().unwrap().len(), 3);

    // --- Test Case 14: g.V(3).inE() ---
    println!("\n--- Testing g.V(3).inE() ---");
    let query_ine = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(3)] },
            ParsedGremlinStep { name: "inE".to_string(), arguments: vec![] },
        ],
    };
    let response_ine = client.send_query(query_ine).await?;
    let data_ine = test_utils::parse_server_response(&response_ine)?;
    assert_eq!(data_ine.as_array().unwrap().len(), 3);

    // --- Test Case 15: g.V(3).in() ---
    println!("\n--- Testing g.V(3).in() ---");
    let query_in = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(3)] },
            ParsedGremlinStep { name: "in".to_string(), arguments: vec![] },
        ],
    };
    let response_in = client.send_query(query_in).await?;
    let data_in = test_utils::parse_server_response(&response_in)?;
    assert_eq!(data_in.as_array().unwrap().len(), 3);

    // --- Test Case 16: g.V(4).both() ---
    println!("\n--- Testing g.V(4).both() ---");
    let query_both = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(4)] },
            ParsedGremlinStep { name: "both".to_string(), arguments: vec![] },
        ],
    };
    let response_both = client.send_query(query_both).await?;
    let data_both = test_utils::parse_server_response(&response_both)?;
    assert_eq!(data_both.as_array().unwrap().len(), 3);

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

    // --- Step 1: Create vertices ---
    println!("\n--- Testing AddV ---");
    let add_alice_query = GremlinQueryAst {
        source: vec![],
        step: vec![ParsedGremlinStep {
            name: "addV".to_string(),
            arguments: vec![
                GremlinArgument::String("person".to_string()),
                GremlinArgument::Int(101),
                GremlinArgument::Map(std::collections::HashMap::from([(
                    "name".to_string(),
                    GremlinArgument::String("Alice".to_string()),
                )])),
            ],
        }],
    };
    client.send_query(add_alice_query).await?;

    let add_bob_query = GremlinQueryAst {
        source: vec![],
        step: vec![ParsedGremlinStep {
            name: "addV".to_string(),
            arguments: vec![
                GremlinArgument::String("person".to_string()),
                GremlinArgument::Int(102),
                GremlinArgument::Map(std::collections::HashMap::from([(
                    "name".to_string(),
                    GremlinArgument::String("Bob".to_string()),
                )])),
            ],
        }],
    };
    client.send_query(add_bob_query).await?;

    // --- Verification 1: Check vertices exist ---
    println!("\n--- Verifying AddV ---");
    let verify_alice_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "values".to_string(),
                arguments: vec![GremlinArgument::String("name".to_string())],
            },
        ],
    };
    let response_alice = client.send_query(verify_alice_query).await?;
    let data_alice = test_utils::parse_server_response(&response_alice)?;
    assert_eq!(data_alice.as_array().unwrap().len(), 1);
    assert_eq!(
        data_alice[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "Alice"
        ))))
    );

    // --- Step 2: Create an edge ---
    println!("\n--- Testing AddE ---");
    let add_edge_query = GremlinQueryAst {
        source: vec![],
        step: vec![ParsedGremlinStep {
            name: "addE".to_string(),
            arguments: vec![
                GremlinArgument::String("knows".to_string()),
                GremlinArgument::Int(101),
                GremlinArgument::Int(102),
                GremlinArgument::Map(std::collections::HashMap::from([(
                    "since".to_string(),
                    GremlinArgument::Int(2020),
                )])),
            ],
        }],
    };
    client.send_query(add_edge_query).await?;

    // --- Verification 2: Check edge exists ---
    println!("\n--- Verifying AddE ---");
    let verify_edge_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "out".to_string(),
                arguments: vec![GremlinArgument::String("knows".to_string())],
            },
        ],
    };
    let response_edge = client.send_query(verify_edge_query).await?;
    let data_edge = test_utils::parse_server_response(&response_edge)?;
    assert_eq!(data_edge.as_array().unwrap().len(), 1);
    assert_eq!(data_edge[0], test_utils::gvalue_to_json_value(&crate::types::GValue::Vertex(102)));

    // --- Step 3: Modify vertex property ---
    println!("\n--- Testing Vertex Property Update ---");
    let update_vertex_prop_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "property".to_string(),
                arguments: vec![
                    GremlinArgument::String("name".to_string()),
                    GremlinArgument::String("Alicia".to_string()),
                ],
            },
        ],
    };
    client.send_query(update_vertex_prop_query).await?;

    // --- Verification 3: Check vertex property update ---
    println!("\n--- Verifying Vertex Property Update ---");
    let verify_update_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "values".to_string(),
                arguments: vec![GremlinArgument::String("name".to_string())],
            },
        ],
    };
    let response_update = client.send_query(verify_update_query).await?;
    let data_update = test_utils::parse_server_response(&response_update)?;
    assert_eq!(
        data_update[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::String(smol_str::SmolStr::new(
            "Alicia"
        ))))
    );

    // --- Step 4: Modify edge property ---
    println!("\n--- Testing Edge Property Update ---");
    let update_edge_prop_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "outE".to_string(),
                arguments: vec![GremlinArgument::String("knows".to_string())],
            },
            ParsedGremlinStep {
                name: "property".to_string(),
                arguments: vec![GremlinArgument::String("since".to_string()), GremlinArgument::Int(2022)],
            },
        ],
    };
    client.send_query(update_edge_prop_query).await?;

    // --- Verification 4: Check edge property update ---
    println!("\n--- Verifying Edge Property Update ---");
    let verify_edge_update_query = GremlinQueryAst {
        source: vec![],
        step: vec![
            ParsedGremlinStep { name: "V".to_string(), arguments: vec![GremlinArgument::Int(101)] },
            ParsedGremlinStep {
                name: "outE".to_string(),
                arguments: vec![GremlinArgument::String("knows".to_string())],
            },
            ParsedGremlinStep {
                name: "values".to_string(),
                arguments: vec![GremlinArgument::String("since".to_string())],
            },
        ],
    };
    let response_edge_update = client.send_query(verify_edge_update_query).await?;
    let data_edge_update = test_utils::parse_server_response(&response_edge_update)?;
    assert_eq!(
        data_edge_update[0],
        test_utils::gvalue_to_json_value(&crate::types::GValue::Scalar(Primitive::Int32(2022)))
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
