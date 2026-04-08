use crate::spawner::container::{ensure, ensure_stack, kill, list, spawn};
use anyhow::Result;

use bollard::Docker;
use colonyos::core::{Executor, Software};
use crypto_box::SecretKey;

use tokio::signal;

const KEY_PATH: &str = ".colony/node.key";

fn load_or_generate_key() -> Result<SecretKey> {
    if let Ok(hex_str) = std::fs::read_to_string(KEY_PATH) {
        let bytes: [u8; 32] = hex::decode(hex_str.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid key length in {KEY_PATH}"))?;
        println!("Loaded encryption key from {KEY_PATH}");
        Ok(SecretKey::from(bytes))
    } else {
        use crypto_box::aead::OsRng;
        let secret = SecretKey::generate(&mut OsRng);
        if let Some(parent) = std::path::Path::new(KEY_PATH).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(KEY_PATH, hex::encode(secret.to_bytes()))?;
        println!("Generated encryption key, saved to {KEY_PATH}");
        Ok(secret)
    }
}

pub async fn run_executor(docker: Docker) -> Result<()> {
    let executor_name = format!("node-agent-{}", hostname::get()?.to_string_lossy());
    let colony_name = "dev";
    let prvkey = "ba949fa134981372d6da62b6a56f336ab4d843b22c02a4257dcf7d0d73097514";
    let exec_prvkey = colonyos::crypto::gen_prvkey();
    let executor_id = colonyos::crypto::gen_id(&exec_prvkey);

    let secret_key = load_or_generate_key()?;
    let pubkey_hex = hex::encode(secret_key.public_key().as_bytes());

    let mut executor = Executor::new(&executor_name, &executor_id, "node-agent", colony_name);
    executor.capabilities.software.push(Software {
        name: "pubkey".into(),
        software_type: pubkey_hex.clone(),
        version: String::new(),
    });

    colonyos::add_executor(&executor, prvkey).await?;
    colonyos::approve_executor(colony_name, &executor_name, prvkey).await?;
    println!("Executor registered with pubkey: {pubkey_hex}");

    let result = run_loop(colony_name, &exec_prvkey, &secret_key, &docker).await;

    println!("Removing executor...");
    colonyos::remove_executor(colony_name, &executor_name, prvkey).await?;
    result
}

async fn run_loop(
    colony_name: &str,
    prvkey: &str,
    secret_key: &SecretKey,
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
                            "spawn"  => spawn(process.spec.kwargs, secret_key, docker).await,
                            "kill"   => kill(process.spec.kwargs, docker).await,
                            "list"   => list(process.spec.kwargs, docker).await,
                            "ensure"        => ensure(process.spec.kwargs, secret_key, docker).await,
                            "ensure_stack"  => ensure_stack(process.spec.kwargs, secret_key, docker).await,
                            _ => Err(anyhow::anyhow!("Unknown function: {}", process.spec.funcname)),
                        };
                        match res {
                            Ok(out) => {
                                if let Err(e) =
                                    colonyos::set_output(&process.processid, out, prvkey).await
                                {
                                    eprintln!("Failed to set output: {e}");
                                } else if let Err(e) =
                                    colonyos::close(&process.processid, prvkey).await
                                {
                                    eprintln!("Failed to close process: {e}");
                                } else {
                                    println!("Process completed successfully");
                                }
                            }
                            Err(e) => {
                                eprintln!("Process {} failed: {e}", process.processid);
                                if let Err(fail_err) =
                                    colonyos::fail(&process.processid, prvkey).await
                                {
                                    eprintln!("Failed to mark process as failed: {fail_err}");
                                }
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
