use crate::{
    engine::volcano::builder::PhysicalPlanBuilder,
    graph::LogicalGraph,
    optimizer::optimize,
    planner::logical_step::LogicalPlan,
    server::{
        bytecode_deserializer::{deserialize_bytecode, GremlinQueryAst},
        config::Config,
        result_serializer::serialize_results,
    },
    store::{GraphStore, RocksStorage},
};
use futures_util::{SinkExt, StreamExt};
use std::{path::Path, sync::Arc};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub fn open_rocks_store<P: AsRef<Path>>(path: Option<P>) -> Result<Arc<RocksStorage>, Box<dyn std::error::Error>> {
    match path {
        Some(pth) => Ok(Arc::new(RocksStorage::open(pth)?)),
        None => {
            let dir = tempfile::tempdir()?;
            Ok(Arc::new(RocksStorage::open(dir.path())?))
        }
    }
}

/// Loads configuration and starts the server using the provided config file path.
pub async fn run_server_with_config<P: AsRef<Path>>(config_path: P) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_file(config_path)?;

    let graph_store = open_rocks_store(Some(&config.storage.data_dir))?;
    let addr = config.addr();

    start_server(&addr, graph_store).await
}

/// Starts the Gremlin WebSocket server.
pub async fn start_server(addr: &str, graph_store: Arc<RocksStorage>) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(addr).await?;
    println!("Gremlin WebSocket server listening on: {}", addr);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let graph_store_clone = Arc::clone(&graph_store);
                tokio::spawn(handle_connection(stream, graph_store_clone));
            }
            Err(e) => {
                // Log the error but continue listening for new connections.
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }
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
                let mut logical_graph = LogicalGraph::new(graph_store.begin());
                let response = process_query_message(text.as_bytes(), &mut logical_graph);
                if let Err(e) = sender.send(Message::Text(response.into())).await {
                    eprintln!("Error sending response: {:?}", e);
                    break;
                }
            }
            Message::Binary(bytes) => {
                let mut logical_graph = LogicalGraph::new(graph_store.begin());
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

    let mut builder: PhysicalPlanBuilder = Default::default();
    let physical_plan = builder.build(&logical_plan);

    let mut results = Vec::new();
    loop {
        match physical_plan.next(graph) {
            Ok(Some(traverser)) => results.push(traverser.as_ref().value.clone()),
            Ok(None) => break,
            Err(e) => {
                let error_msg = serde_json::to_string(&format!("Runtime Error: {e}")).unwrap_or_default();
                return format!(r#"{{"status":{{"code":500,"message":{error_msg}}}}}"#);
            }
        }
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
