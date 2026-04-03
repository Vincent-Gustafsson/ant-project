use crate::reconciler::{cleanup, reconcile};

use anyhow::Result;
use colonyos::core::Executor;
use std::sync::OnceLock;
use tokio::signal;

static EXEC_PRVKEY: OnceLock<String> = OnceLock::new();

pub fn exec_prvkey() -> &'static str {
    EXEC_PRVKEY
        .get()
        .expect("'exec_prvkey' is not initialized.")
}

pub async fn run_executor() -> Result<()> {
    let executor_name = "deployment-controller";
    let colony_name = "dev";
    let colony_prvkey = "ba949fa134981372d6da62b6a56f336ab4d843b22c02a4257dcf7d0d73097514";

    let _ = EXEC_PRVKEY.set(colonyos::crypto::gen_prvkey());
    let executor_id = colonyos::crypto::gen_id(exec_prvkey());
    let executor = Executor::new(&executor_name, &executor_id, &executor_name, colony_name);

    colonyos::add_executor(&executor, colony_prvkey).await?;
    colonyos::approve_executor(colony_name, &executor_name, colony_prvkey).await?;

    println!("Executor registered, waiting for processes...");
    let result = run_loop(colony_name).await;

    println!("Removing executor...");
    colonyos::remove_executor(colony_name, &executor_name, exec_prvkey()).await?;
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
