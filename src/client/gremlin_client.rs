use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

/// A simplified representation of Gremlin Bytecode for client-side construction.
/// This mirrors the server's `GremlinQueryAst` for easy serialization.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum GremlinArgument {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    #[serde(rename = "bytecode")]
    NestedBytecode(GremlinQueryAst),
    List(Vec<GremlinArgument>),
    Map(std::collections::HashMap<String, GremlinArgument>),
}

use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ParsedGremlinStep {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<GremlinArgument>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GremlinQueryAst {
    #[serde(default)]
    pub step: Vec<ParsedGremlinStep>,
}

/// A simple Gremlin client to connect to the WebSocket server and send queries.
pub struct GremlinClient<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> {
    ws_sender: SplitSink<WebSocketStream<S>, Message>,
    ws_receiver: SplitStream<WebSocketStream<S>>,
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
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> GremlinClient<S> {
    /// Sends a WebSocket Ping and waits for the server's Pong, returning the echoed payload.
    /// Use this to verify WebSocket connectivity before sending queries.
    pub async fn send_ping(&mut self, payload: Vec<u8>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.ws_sender.send(Message::Ping(payload.into())).await?;
        while let Some(msg) = self.ws_receiver.next().await {
            if let Message::Pong(pong_payload) = msg? {
                return Ok(pong_payload.to_vec());
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

pub type DefaultStream = MaybeTlsStream<tokio::net::TcpStream>;

// ── Fluent Query Builder ──────────────────────────────────────────────────────

#[allow(non_snake_case)]
pub fn graphTraversalSource<'a, S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    client: &'a mut GremlinClient<S>,
) -> GraphTraversal<'a, S> {
    GraphTraversal { client: Some(Rc::new(RefCell::new(client))), ast: GremlinQueryAst { step: vec![] } }
}

/// Entry point for anonymous traversals (sub-traversals).
/// Mimics Gremlin's `__` (double underscore) for nested traversals.
pub fn __() -> GraphTraversal<'static, DefaultStream> {
    GraphTraversal { client: None, ast: GremlinQueryAst { step: vec![] } }
}

#[derive(Clone)]
pub struct GraphTraversal<'a, S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin = DefaultStream> {
    client: Option<Rc<RefCell<&'a mut GremlinClient<S>>>>,
    ast: GremlinQueryAst,
}

#[allow(non_snake_case)]
impl<'a, S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> GraphTraversal<'a, S> {
    pub fn reset(&mut self) -> &mut Self {
        self.ast.step.clear();
        self
    }
    /// Executes the traversal against the bound client and waits for the response.
    #[allow(clippy::await_holding_refcell_ref)]
    pub async fn execute(&mut self) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let response_text = if let Some(client) = &self.client {
            client.borrow_mut().send_query(self.ast.clone()).await?
        } else {
            return Err("Cannot execute an anonymous traversal".into());
        };

        let parsed: serde_json::Value = serde_json::from_str(&response_text)?;

        if parsed["status"]["code"] != 200 {
            let msg = parsed["status"]["message"].as_str().unwrap_or("Unknown error");
            return Err(format!("Server returned error ({}): {}", parsed["status"]["code"], msg).into());
        }

        let data = parsed["result"]["data"]["@value"].clone();
        Ok(data)
    }

    pub fn has(&mut self, key: &str, value: GremlinArgument) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep {
            name: "has".to_string(),
            arguments: vec![GremlinArgument::String(key.to_string()), value],
        });
        self
    }

    /// Spawns a traversal with the `V()` step.
    /// This method is available on `GraphTraversal` for sub-traversals (e.g., `__.V()`).
    pub fn V(&mut self, ids: &[i64]) -> &mut Self {
        let arguments = ids.iter().map(|&i| GremlinArgument::Int(i)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "V".to_string(), arguments });
        self
    }

    pub fn addV(
        &mut self,
        label_id: u32,
        vertex_id: i64,
        properties: std::collections::HashMap<String, GremlinArgument>,
    ) -> &mut Self {
        let arguments = vec![
            GremlinArgument::Int(label_id as i64),
            GremlinArgument::Int(vertex_id),
            GremlinArgument::Map(properties),
        ];
        self.ast.step.push(ParsedGremlinStep { name: "addV".to_string(), arguments });
        self
    }

    pub fn addE(
        &mut self,
        label_id: u32,
        out_v_id: i64,
        in_v_id: i64,
        properties: std::collections::HashMap<String, GremlinArgument>,
    ) -> &mut Self {
        let arguments = vec![
            GremlinArgument::Int(label_id as i64),
            GremlinArgument::Int(out_v_id),
            GremlinArgument::Int(in_v_id),
            GremlinArgument::Map(properties),
        ];
        self.ast.step.push(ParsedGremlinStep { name: "addE".to_string(), arguments });
        self
    }

    pub fn out(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "out".to_string(), arguments });
        self
    }

    pub fn outE(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "outE".to_string(), arguments });
        self
    }

    pub fn r#in(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "in".to_string(), arguments });
        self
    }

    pub fn inE(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "inE".to_string(), arguments });
        self
    }

    pub fn both(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "both".to_string(), arguments });
        self
    }

    pub fn bothE(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "bothE".to_string(), arguments });
        self
    }

    pub fn count(&mut self) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep { name: "count".to_string(), arguments: vec![] });
        self
    }

    pub fn hasLabel(&mut self, labels: &[u32]) -> &mut Self {
        let arguments = labels.iter().map(|&l| GremlinArgument::Int(l as i64)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "hasLabel".to_string(), arguments });
        self
    }

    pub fn inV(&mut self) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep { name: "inV".to_string(), arguments: vec![] });
        self
    }

    pub fn otherV(&mut self) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep { name: "otherV".to_string(), arguments: vec![] });
        self
    }

    pub fn outV(&mut self) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep { name: "outV".to_string(), arguments: vec![] });
        self
    }

    pub fn is(&mut self, value: GremlinArgument) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep { name: "is".to_string(), arguments: vec![value] });
        self
    }

    pub fn property(&mut self, key: &str, value: GremlinArgument) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep {
            name: "property".to_string(),
            arguments: vec![GremlinArgument::String(key.to_string()), value],
        });
        self
    }

    pub fn values(&mut self, keys: &[&str]) -> &mut Self {
        let arguments = keys.iter().map(|&s| GremlinArgument::String(s.to_string())).collect();
        self.ast.step.push(ParsedGremlinStep { name: "values".to_string(), arguments });
        self
    }

    pub fn r#where(&mut self, traversal: &mut GraphTraversal<'_, S>) -> &mut Self {
        self.ast.step.push(ParsedGremlinStep {
            name: "where".to_string(),
            arguments: vec![GremlinArgument::NestedBytecode(traversal.build())],
        });
        self
    }

    pub fn union(&mut self, traversals: Vec<&mut GraphTraversal<'_, S>>) -> &mut Self {
        let arguments = traversals.into_iter().map(|t| GremlinArgument::NestedBytecode(t.build())).collect();
        self.ast.step.push(ParsedGremlinStep { name: "union".to_string(), arguments });
        self
    }

    pub fn coalesce(&mut self, traversals: Vec<&mut GraphTraversal<'_, S>>) -> &mut Self {
        let arguments = traversals.into_iter().map(|t| GremlinArgument::NestedBytecode(t.build())).collect();
        self.ast.step.push(ParsedGremlinStep { name: "coalesce".to_string(), arguments });
        self
    }
    pub fn limit(&mut self, limit: u32) -> &mut Self {
        self.ast
            .step
            .push(ParsedGremlinStep { name: "limit".to_string(), arguments: vec![GremlinArgument::Int(limit as i64)] });
        self
    }
    pub fn hasId(&mut self, ids: &[i64]) -> &mut Self {
        let arguments = ids.iter().map(|&i| GremlinArgument::Int(i)).collect();
        self.ast.step.push(ParsedGremlinStep { name: "hasId".to_string(), arguments });
        self
    }
    /// Extracts the fully built AST to send over the network.
    pub fn build(&self) -> GremlinQueryAst {
        self.ast.clone()
    }
}

/// Example usage of the Gremlin client.
#[tokio::main(flavor = "current_thread")]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = "127.0.0.1:8080"; // Default address for the server
    let mut client = GremlinClient::connect(server_addr).await?;
    let mut g = graphTraversalSource(&mut client);

    // Example 1: g.V(1, 4)
    println!("\nSending query: g.V(1, 4)");
    let data_v = g.V(&[1, 4]).execute().await?;
    println!("Results: {}", data_v);

    // Example 2: g.V().has('name', 'marko').outE().count()
    println!("\nSending query: g.V().has('name', 'marko').outE().count()");
    g.reset();
    let count_data = g.V(&[]).has("name", GremlinArgument::String("marko".into())).outE(&[]).count().execute().await?;
    println!("Count result: {}", count_data);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fluent_builder_serialization_deserialization() {
        let mut builder = __();
        let query = builder
            .V(&[1, 4])
            .has("name", GremlinArgument::String("marko".into())) // Corrected to pass GremlinArgument
            .out(&[3])
            .outE(&[4, 5])
            .r#in(&[])
            .inE(&[1])
            .both(&[2])
            .bothE(&[])
            .values(&["age", "name"]) // Corrected to pass slice
            .count()
            .build();

        // 1. Serialize the query AST to a JSON string (Client side behavior)
        let json = serde_json::to_string(&query).expect("Failed to serialize query AST");

        // 2. Deserialize it back (Simulating Server side receiving the JSON bytecode)
        let deserialized: GremlinQueryAst = serde_json::from_str(&json).expect("Failed to deserialize query AST");

        assert_eq!(deserialized.step.len(), 10);

        // Verify V
        assert_eq!(deserialized.step[0].name, "V");
        assert!(matches!(&deserialized.step[0].arguments[0], GremlinArgument::Int(1)));
        assert!(matches!(&deserialized.step[0].arguments[1], GremlinArgument::Int(4)));

        // Verify has
        assert_eq!(deserialized.step[1].name, "has");
        assert!(matches!(&deserialized.step[1].arguments[0], GremlinArgument::String(s) if s == "name"));
        assert!(matches!(&deserialized.step[1].arguments[1], GremlinArgument::String(s) if s == "marko"));

        // Verify out
        assert_eq!(deserialized.step[2].name, "out");
        assert!(matches!(&deserialized.step[2].arguments[0], GremlinArgument::Int(3)));

        // Verify outE
        assert_eq!(deserialized.step[3].name, "outE");
        assert!(matches!(&deserialized.step[3].arguments[0], GremlinArgument::Int(4)));
        assert!(matches!(&deserialized.step[3].arguments[1], GremlinArgument::Int(5)));

        // Verify empty labels
        assert_eq!(deserialized.step[4].name, "in");
        assert!(deserialized.step[4].arguments.is_empty());

        // Verify values
        assert_eq!(deserialized.step[8].name, "values");
        assert!(matches!(&deserialized.step[8].arguments[0], GremlinArgument::String(s) if s == "age"));
        assert!(matches!(&deserialized.step[8].arguments[1], GremlinArgument::String(s) if s == "name"));

        // Verify count
        assert_eq!(deserialized.step[9].name, "count");
        assert!(deserialized.step[9].arguments.is_empty());
    }

    #[test]
    fn test_fluent_builder_complex_query_serialization_deserialization() {
        use std::collections::HashMap;
        let mut props = HashMap::new();
        props.insert("name".to_string(), GremlinArgument::String("Alice".to_string()));
        props.insert("age".to_string(), GremlinArgument::Int(30));

        // Sub-traversals now correctly use __()
        let mut sub_traversal_where = __();
        sub_traversal_where.V(&[]).hasLabel(&[1]); // __.V().hasLabel(1)
        let mut sub_traversal_union_1 = __();
        sub_traversal_union_1.out(&[3]).count(); // __.out(3).count()
        let mut sub_traversal_union_2 = __();
        sub_traversal_union_2.r#in(&[4]).values(&["name"]); // __.in(4).values("name")
        let mut query_builder = __();
        let query = query_builder
            .addV(1, 101, props.clone()) // person, id 101, name Alice, age 30 (now starts with addV)
            .addE(3, 101, 102, HashMap::from([("weight".to_string(), GremlinArgument::Float(0.5))])) // knows, from 101 to 102, weight 0.5
            .has("status", GremlinArgument::String("active".to_string()))
            .hasLabel(&[1, 2]) // person or software
            .inV()
            .otherV()
            .outV()
            .is(GremlinArgument::Int(101))
            .property("last_updated", GremlinArgument::Int(2023))
            .r#where(&mut sub_traversal_where)
            .union(vec![&mut sub_traversal_union_1, &mut sub_traversal_union_2])
            .build();

        let json = serde_json::to_string(&query).expect("Failed to serialize complex query AST");
        let deserialized: GremlinQueryAst =
            serde_json::from_str(&json).expect("Failed to deserialize complex query AST");

        assert_eq!(deserialized.step.len(), 11); // V(&[100]) removed

        // Verify addV
        assert_eq!(deserialized.step[0].name, "addV"); // Index 0
        assert!(matches!(&deserialized.step[0].arguments[0], GremlinArgument::Int(1)));
        assert!(matches!(&deserialized.step[0].arguments[1], GremlinArgument::Int(101)));
        assert!(matches!(&deserialized.step[0].arguments[2], GremlinArgument::Map(_)));

        // Verify addE
        assert_eq!(deserialized.step[1].name, "addE"); // Index 1
        assert!(matches!(&deserialized.step[1].arguments[0], GremlinArgument::Int(3)));
        assert!(matches!(&deserialized.step[1].arguments[1], GremlinArgument::Int(101)));
        assert!(matches!(&deserialized.step[1].arguments[2], GremlinArgument::Int(102)));
        assert!(matches!(&deserialized.step[1].arguments[3], GremlinArgument::Map(_)));

        // Verify has with GremlinArgument
        assert_eq!(deserialized.step[2].name, "has"); // Index 2
        assert!(matches!(&deserialized.step[2].arguments[0], GremlinArgument::String(s) if s == "status"));
        assert!(matches!(&deserialized.step[2].arguments[1], GremlinArgument::String(s) if s == "active"));

        // Verify hasLabel
        assert_eq!(deserialized.step[3].name, "hasLabel"); // Index 3
        assert!(matches!(&deserialized.step[3].arguments[0], GremlinArgument::Int(1)));
        assert!(matches!(&deserialized.step[3].arguments[1], GremlinArgument::Int(2)));

        // Verify inV, otherV, outV
        assert_eq!(deserialized.step[4].name, "inV"); // Index 4
        assert_eq!(deserialized.step[5].name, "otherV"); // Index 5
        assert_eq!(deserialized.step[6].name, "outV"); // Index 6

        // Verify is
        assert_eq!(deserialized.step[7].name, "is"); // Index 7
        assert!(matches!(&deserialized.step[7].arguments[0], GremlinArgument::Int(101)));

        // Verify property
        assert_eq!(deserialized.step[8].name, "property"); // Index 8
        assert!(matches!(&deserialized.step[8].arguments[0], GremlinArgument::String(s) if s == "last_updated"));
        assert!(matches!(&deserialized.step[8].arguments[1], GremlinArgument::Int(2023)));

        // Verify where
        assert_eq!(deserialized.step[9].name, "where"); // Index 9
        if let GremlinArgument::NestedBytecode(nested_ast) = &deserialized.step[9].arguments[0] {
            assert_eq!(nested_ast.step.len(), 2);
            assert_eq!(nested_ast.step[0].name, "V"); // V is valid in sub-traversals
            assert_eq!(nested_ast.step[1].name, "hasLabel");
            assert!(matches!(&nested_ast.step[1].arguments[0], GremlinArgument::Int(1)));
        } else {
            panic!("Expected NestedBytecode for where step");
        }

        // Verify union
        assert_eq!(deserialized.step[10].name, "union"); // Index 10
        if let GremlinArgument::NestedBytecode(nested_ast_1) = &deserialized.step[10].arguments[0] {
            assert_eq!(nested_ast_1.step.len(), 2);
            assert_eq!(nested_ast_1.step[0].name, "out");
            assert!(matches!(&nested_ast_1.step[0].arguments[0], GremlinArgument::Int(3)));
            assert_eq!(nested_ast_1.step[1].name, "count");
        } else {
            panic!("Expected NestedBytecode for union sub-traversal 1");
        }
        if let GremlinArgument::NestedBytecode(nested_ast_2) = &deserialized.step[10].arguments[1] {
            assert_eq!(nested_ast_2.step.len(), 2);
            assert_eq!(nested_ast_2.step[0].name, "in");
            assert!(matches!(&nested_ast_2.step[0].arguments[0], GremlinArgument::Int(4)));
            assert_eq!(nested_ast_2.step[1].name, "values");
            assert!(matches!(&nested_ast_2.step[1].arguments[0], GremlinArgument::String(s) if s == "name"));
        } else {
            panic!("Expected NestedBytecode for union sub-traversal 2");
        }
    }
}
