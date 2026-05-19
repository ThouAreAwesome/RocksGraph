use crate::{
    client::gremlin_client::{self, GremlinArgument},
    server::{gremlin_server, test_utils},
};
use rand::Rng;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
};
use tokio::time::{sleep, Duration};

use std::time::Instant;

async fn random_server_addr() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

fn generate_random_string(len: usize) -> String {
    rand::thread_rng().sample_iter(rand::distributions::Alphanumeric).take(len).map(char::from).collect()
}

fn generate_random_properties() -> HashMap<String, GremlinArgument> {
    let mut rng = rand::thread_rng();
    HashMap::from([
        ("name".to_string(), GremlinArgument::String(generate_random_string(10))),
        ("age".to_string(), GremlinArgument::Int(rng.gen_range(18..100))),
    ])
}

fn generate_random_edge_properties() -> HashMap<String, GremlinArgument> {
    let mut rng = rand::thread_rng();
    HashMap::from([
        ("weight".to_string(), GremlinArgument::Float(rng.gen_range(0.1..10.0))),
        ("timestamp".to_string(), GremlinArgument::Int(rng.gen_range(0..1000000))),
    ])
}

async fn check_vertex_exists(
    g: &mut gremlin_client::GraphTraversal<'_>,
    vertex_id: i32,
) -> Result<bool, Box<dyn std::error::Error>> {
    let result = g.reset().V(&[vertex_id]).execute().await?;
    Ok(!result.as_array().unwrap().is_empty())
}

async fn create_vertex_with_retry(
    g: &mut gremlin_client::GraphTraversal<'_>,
    vertex_id: i32,
    max_retries: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 0..max_retries {
        match g.reset().addV(1u32, vertex_id, generate_random_properties()).execute().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                if attempt == max_retries - 1 {
                    return Err(e);
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
    Err("Failed to create vertex after max retries".into())
}

async fn create_edge_with_retry(
    g: &mut gremlin_client::GraphTraversal<'_>,
    src: i32,
    dst: i32,
    edge_type: i32,
    max_retries: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 0..max_retries {
        match g.reset().addE(2u32, src, dst, generate_random_edge_properties()).execute().await {
            Ok(result) => {
                if result.as_array().unwrap().is_empty() {
                    if attempt == max_retries - 1 {
                        return Err("Failed to add edge after max retries".into());
                    }
                    sleep(Duration::from_millis(50)).await;
                } else {
                    return Ok(());
                }
            }
            Err(e) => {
                if attempt == max_retries - 1 {
                    return Err(e);
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
    Err("Failed to create edge after max retries".into())
}

#[tokio::test]
async fn bench_gremlin_server() -> Result<(), Box<dyn std::error::Error>> {
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

    let file = File::open("/Users/bytedance/WorkSpace/MultiGraph/src/server/soc-LiveJournal1.txt")?;
    let reader = BufReader::new(file);

    let start = Instant::now();
    let mut counter = 0;
    let max_retries = 3;

    for line in reader.lines() {
        if counter <= 4 {
            counter += 1;
            continue;
        }
        let line = line.unwrap(); // Handle potential errors during reading

        counter += 1;
        if counter % 10000 == 0 {
            let elapsed = start.elapsed();
            println!("Processing {} lines per second", counter / elapsed.as_secs());
            println!("Read {} lines", counter);
        }

        let parts: Vec<i32> = line
            .split_whitespace()
            .map(|s| s.parse::<i32>())
            .filter_map(Result::ok) // Silently skips strings that fail to parse
            .collect();
        if parts.len() != 2 {
            continue;
        }

        // Check and create source vertex if not exists
        let src_exists = check_vertex_exists(&mut g, parts[0]).await?;
        if !src_exists {
            create_vertex_with_retry(&mut g, parts[0], max_retries).await?;
        }

        // Check and create destination vertex if not exists
        let dst_exists = check_vertex_exists(&mut g, parts[1]).await?;
        if !dst_exists {
            create_vertex_with_retry(&mut g, parts[1], max_retries).await?;
        }

        // Add edge with retry logic
        match create_edge_with_retry(&mut g, parts[0], parts[1], 2, max_retries).await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to add edge from {} to {}: {}", parts[0], parts[1], e);
            }
        }
    }
    Ok(())
}
