use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::{
    engine::volcano::builder::PhysicalPlanBuilder,
    graph::LogicalGraph,
    optimizer::optimize,
    planner::logical_step::LogicalPlan,
    server::{
        bytecode_deserializer::{deserialize_bytecode, GremlinQueryAst},
        result_serializer::serialize_results,
    },
    store::{GraphStore, RocksStorage},
};

/// Starts the Gremlin WebSocket server.
pub async fn start_server(addr: &str, graph_store: Arc<RocksStorage>) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(addr).await?;
    println!("Gremlin WebSocket server listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let graph_store_clone = Arc::clone(&graph_store);
        tokio::spawn(handle_connection(stream, graph_store_clone));
    }
    Ok(())
}

/// Handles a single WebSocket connection from a Gremlin client.
async fn handle_connection(stream: TcpStream, graph_store: Arc<RocksStorage>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WebSocket handshake failed: {:?}", e);
            return;
        }
    };

    println!("New WebSocket connection established.");

    let (mut sender, mut receiver) = ws_stream.split();

    // Each connection gets its own LogicalGraph for transactional context
    let mut logical_graph = LogicalGraph::new(graph_store.begin());

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("Error receiving message: {:?}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                // For simplicity, we'll treat text messages as JSON-encoded bytecode
                // In a real TinkerPop server, this would be binary Gryo.
                println!("Received text message (assuming JSON bytecode): {}", text);
                let response = process_query_message(text.as_bytes(), &mut logical_graph);
                if let Err(e) = sender.send(Message::Text(response.into())).await {
                    eprintln!("Error sending response: {:?}", e);
                    break;
                }
            }
            Message::Binary(bytes) => {
                println!("Received binary message (assuming Gryo bytecode): {:?}", bytes);
                let response = process_query_message(&bytes, &mut logical_graph);
                if let Err(e) = sender.send(Message::Text(response.into())).await {
                    eprintln!("Error sending response: {:?}", e);
                    break;
                }
            }
            Message::Ping(pong) => {
                if let Err(e) = sender.send(Message::Pong(pong)).await {
                    eprintln!("Error sending pong: {:?}", e);
                    break;
                }
            }
            Message::Close(_) => {
                println!("Client disconnected.");
                break;
            }
            _ => {} // Ignore other message types
        }
    }

    // Ensure transaction is aborted if not explicitly committed
    logical_graph.abort();
    println!("WebSocket connection closed.");
}

fn process_query_message(bytes: &[u8], graph: &mut LogicalGraph<RocksStorage>) -> String {
    let ast: GremlinQueryAst = match deserialize_bytecode(bytes) {
        Ok(ast) => ast,
        Err(e) => {
            let error_msg = serde_json::to_string(&format!("Deserialization Error: {:#?}", e)).unwrap_or_default();
            return format!(r#"{{"status":{{"code":400,"message":{}}}}}"#, error_msg);
        }
    };

    let logical_plan: LogicalPlan = match ast.try_into() {
        Ok(plan) => plan,
        Err(e) => {
            let error_msg = serde_json::to_string(&format!("Translation Error: {:#?}", e)).unwrap_or_default();
            return format!(r#"{{"status":{{"code":400,"message":{}}}}}"#, error_msg);
        }
    };
    let logical_plan = optimize(logical_plan);

    let mut builder = PhysicalPlanBuilder::default();
    let physical_plan = builder.build(&logical_plan);

    let mut results = Vec::new();
    while let Some(traverser) = physical_plan.next(graph) {
        results.push(traverser.value);
    }

    // For simplicity, commit after every query. In a real server, this would be explicit.
    if let Err(e) = graph.commit() {
        let error_msg = serde_json::to_string(&format!("Commit Error: {:#?}", e)).unwrap_or_default();
        return format!(r#"{{"status":{{"code":500,"message":{}}}}}"#, error_msg);
    }

    match serialize_results(results) {
        Ok(json) => {
            format!(r#"{{"status":{{"code":200}},"result":{{"data":{{"@type":"g:List","@value":{}}}}}}}"#, json)
        }
        Err(e) => {
            let error_msg = serde_json::to_string(&format!("Serialization Error: {:#?}", e)).unwrap_or_default();
            format!(r#"{{"status":{{"code":500,"message":{}}}}}"#, error_msg)
        }
    }
}
