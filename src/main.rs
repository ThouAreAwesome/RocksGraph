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

use multigraph::server::gremlin_server;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Simple command-line argument parsing for --config
    let config_path = if let Some(pos) = args.iter().position(|arg| arg == "--config") {
        args.get(pos + 1).cloned().expect("A path must be provided after --config")
    } else {
        // Default path if --config is not provided, useful for manual runs.
        "config/server.toml".to_string()
    };

    println!("Starting Gremlin server with config: {}", &config_path);

    tokio::select! {
        // Branch 1: Run the server normally.
        res = gremlin_server::run_server_with_config(&config_path) => {
            if let Err(e) = res {
                eprintln!("Server error: {}", e);
            }
        },
        // Branch 2: Handle Ctrl-C.
        _ = tokio::signal::ctrl_c() => {
            println!("\nCtrl-C received, initiating graceful shutdown.");
        },
        // Branch 3: Handle SIGTERM from `kill` or `make stop`. (Unix-only)
        _ = async {
            #[cfg(unix)]
            {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap().recv().await;
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await; // On non-unix, this future never completes.
        } => {
             println!("\nSIGTERM received, initiating graceful shutdown.");
        }
    }

    println!("Server has shut down.");
    Ok(())
}
