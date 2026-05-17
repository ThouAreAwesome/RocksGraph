use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize}; // `Value` is not used, `json!` is used in the example main
use tokio_tungstenite::{connect_async, tungstenite::Message};
/// A simplified representation of Gremlin Bytecode for client-side construction.
/// This mirrors the server's `GremlinQueryAst` for easy serialization.
#[derive(Debug, Serialize, Deserialize)]
pub enum GremlinArgument {
    String(String),
    Int(i32),
    Float(f64),
    Bool(bool),
    #[serde(rename = "bytecode")]
    NestedBytecode(GremlinQueryAst),
    List(Vec<GremlinArgument>),
    Map(std::collections::HashMap<String, GremlinArgument>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedGremlinStep {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<GremlinArgument>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GremlinQueryAst {
    #[serde(default)]
    pub source: Vec<ParsedGremlinStep>,
    #[serde(default)]
    pub step: Vec<ParsedGremlinStep>,
}

/// A simple Gremlin client to connect to the WebSocket server and send queries.
pub struct GremlinClient<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> {
    ws_sender:
        futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, tokio_tungstenite::tungstenite::Message>,
    ws_receiver: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
}

impl GremlinClient<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    // This impl block is for a specific S
    /// Connects to the Gremlin WebSocket server at the given address.
    pub async fn connect(addr: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let url = url::Url::parse(&format!("ws://{}", addr))?;
        let (ws_stream, _) = connect_async(url.as_str()).await?;
        println!("Connected to Gremlin server at {}", addr);
        let (ws_sender, ws_receiver) = ws_stream.split();
        Ok(GremlinClient { ws_sender, ws_receiver })
    }

    /// Sends a WebSocket Ping and waits for the server's Pong, returning the echoed payload.
    /// Use this to verify WebSocket connectivity before sending queries.
    pub async fn send_ping(&mut self, payload: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.ws_sender.send(Message::Ping(payload.into())).await?;
        while let Some(msg) = self.ws_receiver.next().await {
            match msg? {
                Message::Pong(pong_payload) => return Ok(pong_payload.to_vec()),
                _ => {}
            }
        }
        Err("Connection closed before receiving pong".into())
    }

    /// Sends a Gremlin query (represented as GremlinQueryAst) to the server.
    pub async fn send_query(&mut self, query_ast: GremlinQueryAst) -> Result<String, Box<dyn std::error::Error>> {
        let json_bytecode = serde_json::to_string(&query_ast)?;
        self.ws_sender.send(Message::Text(json_bytecode.into())).await?;

        // Wait for a response
        while let Some(msg) = self.ws_receiver.next().await {
            match msg? {
                Message::Text(response_text) => {
                    return Ok(response_text.to_string());
                }
                Message::Binary(response_bytes) => {
                    // Server currently sends text, but handle binary if it changes
                    return Ok(String::from_utf8(response_bytes.to_vec())?);
                }
                _ => {} // Ignore other message types
            }
        }
        Err("Server disconnected without response".into())
    }
}

/// Example usage of the Gremlin client.
#[tokio::main(flavor = "current_thread")]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = "127.0.0.1:8080"; // Default address for the server
    let mut client = GremlinClient::connect(server_addr).await?;

    // Example 1: g.V(1, 4)
    let query_v = GremlinQueryAst {
        source: vec![ParsedGremlinStep {
            name: "V".to_string(),
            arguments: vec![GremlinArgument::Int(1), GremlinArgument::Int(4)],
        }],
        step: vec![],
    };
    println!("\nSending query: g.V(1, 4)");
    let response_v = client.send_query(query_v).await?;
    println!("Response: {}", response_v);

    // Example 2: g.V().has('name', 'marko').outE().count()
    let query_marko_out_count = GremlinQueryAst {
        source: vec![ParsedGremlinStep { name: "V".to_string(), arguments: vec![] }],
        step: vec![
            ParsedGremlinStep {
                name: "has".to_string(),
                arguments: vec![
                    GremlinArgument::String("name".to_string()),
                    GremlinArgument::String("marko".to_string()),
                ],
            },
            ParsedGremlinStep { name: "outE".to_string(), arguments: vec![] },
            ParsedGremlinStep { name: "count".to_string(), arguments: vec![] },
        ],
    };
    println!("\nSending query: g.V().has('name', 'marko').outE().count()");
    let response_marko_out_count = client.send_query(query_marko_out_count).await?;
    println!("Response: {}", response_marko_out_count);

    Ok(())
}
