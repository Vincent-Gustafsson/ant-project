mod metrics;
mod spawner;

use crate::{metrics::collector::run_metrics_collector, spawner::executor::run_executor};
use anyhow::Result;
use bollard::Docker;

#[tokio::main]
async fn main() -> Result<()> {
    let colony_host = std::env::var("COLONY_HOST").expect("COLONY_HOST env var not set");
    colonyos::set_server_url(&format!("https://{colony_host}/api"));
    let docker = Docker::connect_with_local_defaults()?;

    let enable_metrics = std::env::var("ENABLE_METRICS").map(|v| v == "true").unwrap_or(false);
    if enable_metrics {
        println!("Metrics: ENABLED...");
        tokio::try_join!(
            run_executor(docker.clone()),
            run_metrics_collector(docker.clone())
        )?;
    } else {
        println!("Metrics: DISABLED...");
        run_executor(docker).await?;
    }

    Ok(())
}
