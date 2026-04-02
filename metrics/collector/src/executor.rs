use anyhow::{Context, Result};
use colonyos::core::{Blueprint, Executor, Process};

use questdb::ingress::{Buffer, Sender, TimestampNanos};
use serde::Deserialize;
use tokio::signal;

#[derive(Debug, Deserialize)]
pub struct CpuMetrics {
    pub usage_percent: f32,
    pub core_usages: Vec<f32>,
    pub core_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct MemoryMetrics {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct DiskMetrics {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f32,
}

#[derive(Debug, Deserialize)]
pub struct SystemMetrics {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disks: Vec<DiskMetrics>,
}

pub async fn run_executor() -> Result<()> {
    let executor_name = "metrics-collector";
    let colony_name = "dev";
    let colony_prvkey = "ba949fa134981372d6da62b6a56f336ab4d843b22c02a4257dcf7d0d73097514";

    let exec_prvkey = colonyos::crypto::gen_prvkey();
    let executor_id = colonyos::crypto::gen_id(&exec_prvkey);
    let executor = Executor::new(&executor_name, &executor_id, &executor_name, colony_name);

    colonyos::add_executor(&executor, colony_prvkey).await?;
    colonyos::approve_executor(colony_name, &executor_name, colony_prvkey).await?;

    println!("Executor registered, waiting for processes...");
    let result = run_loop(colony_name, &exec_prvkey).await;

    println!("Removing executor...");
    colonyos::remove_executor(colony_name, &executor_name, colony_prvkey).await?;
    result
}

async fn run_loop(colony_name: &str, prvkey: &str) -> Result<()> {
    let mut sender = Sender::from_conf("http::addr=questdb:9000;")?;

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
                            "push_metrics" => push_metrics(&process, &mut sender),
                            _ => Err(anyhow::anyhow!("Unknown function: {}", process.spec.funcname)),
                        };
                        match res {
                            Ok(out) => {
                                colonyos::set_output(&process.processid, out, prvkey).await?;
                                colonyos::close(&process.processid, prvkey).await?;
                                println!("Process completed successfully");
                            }
                            Err(e) => {
                                println!("{e}");
                                if let Ok(new_sender) = Sender::from_conf("tcp::addr=questdb:9009;protocol_version=2;") {
                                    sender = new_sender;
                                }
                                colonyos::fail(&process.processid, prvkey).await?;
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

fn push_metrics(proc: &Process, sender: &mut Sender) -> Result<Vec<String>> {
    let raw = proc.spec.args.first().context("missing metrics")?;
    let metrics: SystemMetrics = serde_json::from_str(raw)?;

    let mut buf = sender.new_buffer();
    let ts = TimestampNanos::now();

    buf.table("cpu_metrics")?
        .column_f64("usage_percent", metrics.cpu.usage_percent as f64)?
        .column_i64("core_count", metrics.cpu.core_count as i64)?
        .at(ts)?;

    for (id, &usage) in metrics.cpu.core_usages.iter().enumerate() {
        buf.table("cpu_core_metrics")?
            .column_i64("core_id", id as i64)?
            .column_f64("usage_percent", usage as f64)?
            .at(ts)?;
    }

    buf.table("memory_metrics")?
        .column_i64("total_bytes", metrics.memory.total_bytes as i64)?
        .column_i64("used_bytes", metrics.memory.used_bytes as i64)?
        .column_i64("available_bytes", metrics.memory.available_bytes as i64)?
        .column_i64("swap_total_bytes", metrics.memory.swap_total_bytes as i64)?
        .column_i64("swap_used_bytes", metrics.memory.swap_used_bytes as i64)?
        .at(ts)?;

    for disk in &metrics.disks {
        buf.table("disk_metrics")?
            .symbol("name", &disk.name)?
            .symbol("mount_point", &disk.mount_point)?
            .column_i64("total_bytes", disk.total_bytes as i64)?
            .column_i64("used_bytes", disk.used_bytes as i64)?
            .column_i64("available_bytes", disk.available_bytes as i64)?
            .column_f64("usage_percent", disk.usage_percent as f64)?
            .at(ts)?;
    }

    sender.flush(&mut buf)?;

    println!("inserted metrics into db");
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    LÄGG IN (((((((NODNAMNET))))))) AGGGGGDGAWIUDIIAWU
    Ok(vec![])
}
