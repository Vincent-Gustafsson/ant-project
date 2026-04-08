mod metrics;
mod spawner;

use crate::{metrics::collector::run_metrics_collector, spawner::executor::run_executor};
use anyhow::Result;
use bollard::Docker;

#[tokio::main]
async fn main() -> Result<()> {
    colonyos::set_server_url("https://colony.colonypm.xyz:443/api");
    let docker = Docker::connect_with_local_defaults()?;

    let enable_metrics = std::env::var("NA_METRICS_ENABLE").is_ok();
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
