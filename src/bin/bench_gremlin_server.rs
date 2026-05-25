use multigraph::{
    client::gremlin_client::{self, GremlinArgument},
    server::gremlin_server,
};

use rand::Rng;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    sync::mpsc,
    time::{sleep, Duration},
};

const MAX_RETRIES: usize = 3;
const RETRY_DELAY_MS: u64 = 5;
const PARALLELISM: usize = 20;

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
    vertex_id: i64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let result = g.reset().V(&[vertex_id]).execute().await?;
    Ok(!result.as_array().unwrap().is_empty())
}

async fn create_vertex_with_retry(
    g: &mut gremlin_client::GraphTraversal<'_>,
    vertex_id: i64,
    max_retries: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 0..max_retries {
        match g.reset().addV(1u32, vertex_id, generate_random_properties()).execute().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                if attempt == max_retries - 1 {
                    return Err(e);
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
            }
        }
    }
    Err("Failed to create vertex after max retries".into())
}

async fn create_edge_with_retry(
    g: &mut gremlin_client::GraphTraversal<'_>,
    src: i64,
    dst: i64,
    edge_type: u32,
    max_retries: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 0..max_retries {
        match g.reset().addE(edge_type, src, dst, generate_random_edge_properties()).execute().await {
            Ok(result) => {
                if result.as_array().unwrap().is_empty() {
                    if attempt == max_retries - 1 {
                        return Err("Failed to add edge after max retries".into());
                    }
                    sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                } else {
                    return Ok(());
                }
            }
            Err(e) => {
                if attempt == max_retries - 1 {
                    return Err(e);
                }
                sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
            }
        }
    }
    Err("Failed to create edge after max retries".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_addr = random_server_addr().await;

    let path_str = "./bench_data/rocksdb_data";
    let path = PathBuf::from(path_str);
    // 1. Setup: Create a temporary, empty RocksDB store
    let graph_store = gremlin_server::open_rocks_store(Some(&path));

    // 2. Start the Gremlin server in a background task
    let addr_clone = server_addr.clone();
    tokio::spawn(async move {
        gremlin_server::start_server(&addr_clone, graph_store).await.expect("Server failed to start");
    });

    sleep(Duration::from_millis(100)).await;

    let file = File::open("./bench_data/soc-LiveJournal1-1M.txt")?;
    let reader = BufReader::new(file);

    let start = Instant::now();
    let counter = Arc::new(AtomicUsize::new(0));
    let mutation_counter = Arc::new(AtomicUsize::new(0));
    let retrieval_counter = Arc::new(AtomicUsize::new(0));

    // Create a channel for distributing work
    let (tx, rx) = mpsc::channel::<String>(1000);
    let rx = Arc::new(tokio::sync::Mutex::new(rx));

    // 3. Spawn Worker Pool
    // We use std::thread with a current_thread runtime for each worker because the
    // GremlinClient traversal implementation uses Rc/RefCell (non-Send).
    // This allows us to achieve parallelism while keeping the !Send types thread-local.
    let mut worker_handles = vec![];
    for _ in 0..PARALLELISM {
        let rx = Arc::clone(&rx);
        let server_addr = server_addr.clone();
        let mutation_counter = Arc::clone(&mutation_counter);
        let retrieval_counter = Arc::clone(&retrieval_counter);

        let handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            rt.block_on(async move {
                let mut client = match gremlin_client::GremlinClient::connect(&server_addr).await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Worker failed to connect: {}", e);
                        return;
                    }
                };
                let mut g = gremlin_client::graphTraversalSource(&mut client);

                loop {
                    let line = {
                        let mut lock = rx.lock().await;
                        lock.recv().await
                    };

                    let line = match line {
                        Some(l) => l,
                        None => break, // Channel closed
                    };

                    let parts: Vec<i64> =
                        line.split_whitespace().map(|s| s.parse::<i64>()).filter_map(Result::ok).collect();

                    if parts.len() != 2 {
                        continue;
                    }

                    // Processing logic
                    let src_exists = check_vertex_exists(&mut g, parts[0]).await.unwrap_or(false);
                    retrieval_counter.fetch_add(1, Ordering::Relaxed);
                    if !src_exists {
                        let _ = create_vertex_with_retry(&mut g, parts[0], MAX_RETRIES).await;
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    }

                    let dst_exists = check_vertex_exists(&mut g, parts[1]).await.unwrap_or(false);
                    retrieval_counter.fetch_add(1, Ordering::Relaxed);
                    if !dst_exists {
                        let _ = create_vertex_with_retry(&mut g, parts[1], MAX_RETRIES).await;
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    }

                    if create_edge_with_retry(&mut g, parts[0], parts[1], 2, MAX_RETRIES).await.is_ok() {
                        mutation_counter.fetch_add(1, Ordering::Relaxed);
                    } else {
                        continue;
                    }
                }
            })
        });
        worker_handles.push(handle);
    }

    for line in reader.lines() {
        let line = line?;
        tx.send(line).await?;

        let current_count = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if current_count % 10000 == 0 {
            let elapsed = start.elapsed().as_secs().max(1);
            let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
            let r_count = retrieval_counter.load(Ordering::Relaxed) as u64;

            println!("Read {} lines", current_count);
            println!(
                "mutation speed: {}, retrieval speed: {}, total speed: {}",
                m_count / elapsed,
                r_count / elapsed,
                (m_count + r_count) / elapsed
            );
        }
    }

    drop(tx); // Close channel so workers finish
    for handle in worker_handles {
        let _ = handle.join();
    }

    let elapsed = start.elapsed().as_secs().max(1);
    let m_count = mutation_counter.load(Ordering::Relaxed) as u64;
    let r_count = retrieval_counter.load(Ordering::Relaxed) as u64;
    println!(
        "Final speeds - mutation: {}, retrieval: {}, total: {}",
        m_count / elapsed,
        r_count / elapsed,
        (m_count + r_count) / elapsed
    );

    Ok(())
}
