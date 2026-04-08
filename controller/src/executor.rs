use crate::reconciler::{cleanup, reconcile};

use anyhow::Result;
use colonyos::core::{
    BlueprintDefinition, BlueprintDefinitionHandler, BlueprintDefinitionNames,
    BlueprintDefinitionSpec, BlueprintMetadata, Executor, Software,
};
use crypto_box::SecretKey;
use std::sync::OnceLock;
use tokio::signal;

const COLONY_NAME: &str = "dev";
const COLONY_PRVKEY: &str = "ba949fa134981372d6da62b6a56f336ab4d843b22c02a4257dcf7d0d73097514";
const EXECUTOR_NAME: &str = "deployment-controller";
const KEY_PATH: &str = ".colony/controller.key";

static EXEC_PRVKEY: OnceLock<String> = OnceLock::new();
static ENCRYPTION_KEY: OnceLock<SecretKey> = OnceLock::new();

pub fn exec_prvkey() -> &'static str {
    EXEC_PRVKEY
        .get()
        .expect("'exec_prvkey' is not initialized.")
}

pub fn encryption_key() -> &'static SecretKey {
    ENCRYPTION_KEY
        .get()
        .expect("'encryption_key' is not initialized.")
}

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

fn make_bp_definition(name: &str, kind: &str, plural: &str) -> BlueprintDefinition {
    BlueprintDefinition {
        metadata: BlueprintMetadata {
            name: name.to_string(),
            colonyname: COLONY_NAME.to_string(),
            ..Default::default()
        },
        spec: BlueprintDefinitionSpec {
            names: BlueprintDefinitionNames {
                kind: kind.to_string(),
                plural: plural.to_string(),
                ..Default::default()
            },
            handler: BlueprintDefinitionHandler {
                executor_type: EXECUTOR_NAME.to_string(),
                function_name: "reconcile".to_string(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn ensure_bp_definition(name: &str, kind: &str, plural: &str) -> Result<()> {
    if colonyos::get_blueprint_definition(COLONY_NAME, name, COLONY_PRVKEY)
        .await
        .is_err()
    {
        colonyos::add_blueprint_definition(&make_bp_definition(name, kind, plural), COLONY_PRVKEY)
            .await?;
        println!("Registered BlueprintDefinition: {kind}");
    }
    Ok(())
}

pub async fn run_executor() -> Result<()> {
    let _ = EXEC_PRVKEY.set(colonyos::crypto::gen_prvkey());
    let executor_id = colonyos::crypto::gen_id(exec_prvkey());

    let enc_key = load_or_generate_key()?;
    let enc_pubkey = enc_key.public_key();
    let pubkey_hex = hex::encode(enc_pubkey.as_bytes());
    let _ = ENCRYPTION_KEY.set(enc_key);

    let mut executor = Executor::new(EXECUTOR_NAME, &executor_id, EXECUTOR_NAME, COLONY_NAME);
    executor.capabilities.software.push(Software {
        name: "pubkey".into(),
        software_type: pubkey_hex.clone(),
        version: String::new(),
    });

    colonyos::add_executor(&executor, COLONY_PRVKEY).await?;
    colonyos::approve_executor(COLONY_NAME, EXECUTOR_NAME, COLONY_PRVKEY).await?;

    ensure_bp_definition("ant-deployment", "Deployment", "deployments").await?;

    println!("Executor registered with pubkey: {pubkey_hex}");
    println!("Waiting for processes...");
    let result = run_loop(COLONY_NAME).await;

    println!("Removing executor...");
    colonyos::remove_executor(COLONY_NAME, EXECUTOR_NAME, exec_prvkey()).await?;
    result
}

async fn run_loop(colony_name: &str) -> Result<()> {
    let exec_prvkey = exec_prvkey();
    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                println!("Shutting down...");
                return Ok(());
            }
            result = colonyos::assign(colony_name, 10, exec_prvkey) => {
                match result {
                    Ok(process) => {
                        println!("Assigned process: {}", process.processid);
                        let res: Result<Vec<String>> = match process.spec.funcname.as_str() {
                            "reconcile" => reconcile(&process).await,
                            "cleanup" => cleanup(&process).await,
                            _ => Err(anyhow::anyhow!("Unknown function: {}", process.spec.funcname)),
                        };
                        match res {
                            Ok(out) => {
                                colonyos::set_output(&process.processid, out, exec_prvkey).await?;
                                colonyos::close(&process.processid, exec_prvkey).await?;
                                println!("Process completed successfully");
                            }
                            Err(e) => {
                                println!("{e}");
                                colonyos::fail(&process.processid, exec_prvkey).await?;
                            }
                        }
                    }
                    Err(e) if !e.conn_err() => {
                        continue
                    },
                    Err(e) => {
                        eprintln!("Connection error: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }
}
