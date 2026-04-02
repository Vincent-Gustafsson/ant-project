use crate::spawner::container::{kill, list, spawn};
use age::{Decryptor, secrecy::ExposeSecret, x25519};
use anyhow::Result;

use bollard::Docker;
use colonyos::core::{Executor, Software};

use std::{collections::HashMap, str::FromStr};
use tokio::signal;

const KEY_PATH: &str = ".colony/node.key";

fn load_or_generate_identity() -> Result<x25519::Identity> {
    if let Ok(pem) = std::fs::read_to_string(KEY_PATH) {
        let identity = x25519::Identity::from_str(pem.trim())
            .map_err(|e| anyhow::anyhow!("failed to parse identity: {e}"))?;
        println!("Loaded existing identity from {KEY_PATH}");
        Ok(identity)
    } else {
        let identity = x25519::Identity::generate();
        if let Some(parent) = std::path::Path::new(KEY_PATH).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(KEY_PATH, identity.to_string().expose_secret())?;
        println!("Generated new identity, saved to {KEY_PATH}");
        Ok(identity)
    }
}

fn pubkey_software(identity: &x25519::Identity) -> Software {
    Software {
        name: "pubkey".into(),
        software_type: identity.to_public().to_string(),
        version: String::new(),
    }
}

pub async fn run_executor(docker: Docker) -> Result<()> {
    let executor_name = format!("node-agent-{}", hostname::get()?.to_string_lossy());
    let colony_name = "dev";
    let prvkey = "ba949fa134981372d6da62b6a56f336ab4d843b22c02a4257dcf7d0d73097514";
    let executor_id = colonyos::crypto::gen_id(prvkey);

    // Generate or load persistent identity
    let identity = load_or_generate_identity()?;

    let mut executor = Executor::new(&executor_name, &executor_id, "node-agent", colony_name);
    executor
        .capabilities
        .software
        .push(pubkey_software(&identity));

    colonyos::add_executor(&executor, prvkey).await?;
    colonyos::approve_executor(colony_name, &executor_name, prvkey).await?;
    println!("Executor registered with pubkey: {}", identity.to_public());

    let result = run_loop(colony_name, prvkey, &identity, &docker).await;

    println!("Removing executor...");
    colonyos::remove_executor(colony_name, &executor_name, prvkey).await?;
    result
}

async fn run_loop(
    colony_name: &str,
    prvkey: &str,
    identity: &x25519::Identity,
    docker: &Docker,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                println!("Shutting down...");
                return Ok(());
            }
            result = colonyos::assign(colony_name, 10, prvkey) => {
                match result {
                    Ok(process) => {
                        println!("Assigned process: {}", process.processid);
                        let res: Result<Vec<String>> = match process.spec.funcname.as_str() {
                            "spawn" => spawn(process.spec.kwargs, identity, docker).await,
                            "kill"  => kill(process.spec.kwargs, docker).await,
                            "list"  => list(process.spec.kwargs, docker).await,
                            _ => Err(anyhow::anyhow!("Unknown function: {}", process.spec.funcname)),
                        };
                        match res {
                            Ok(out) => {
                                colonyos::set_output(&process.processid, out, prvkey).await?;
                                colonyos::close(&process.processid, prvkey).await?;
                                println!("Process completed successfully");
                            }
                            Err(e) => {
                                colonyos::fail(&process.processid, prvkey).await?;
                                eprintln!("{e}");
                            }
                        }
                    }
                    Err(e) if !e.conn_err() => continue,
                    Err(e) => {
                        eprintln!("Connection error: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }
}
