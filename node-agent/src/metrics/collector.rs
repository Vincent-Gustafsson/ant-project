use std::time::Duration;

use anyhow::Result;
use bollard::Docker;
use chrono::Local;
use colonyos::core::FunctionSpec;
use sysinfo::{Disks, System};
use tokio::time::sleep;

use crate::metrics::system::collect;

pub async fn run_metrics_collector(_docker: Docker) -> Result<()> {
    let mut sys = System::new_all();
    let mut disks = Disks::new_with_refreshed_list();
    let node_name = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string());

    // Prime the CPU delta
    sys.refresh_cpu_all();
    sleep(Duration::from_millis(200)).await;

    loop {
        println!(
            "{} Collecting metrics...",
            Local::now().format("[%H:%M:%S]")
        );

        let snapshot = collect(&mut sys, &mut disks, &node_name);

        let colony_name = std::env::var("COLONY").expect("COLONY env var not set");
        let prvkey = std::env::var("COLONY_PRIVATE_KEY").expect("COLONY_PRIVATE_KEY env var not set");
        let mut fn_spec = FunctionSpec::new("push_metrics", "metrics-collector", &colony_name);
        fn_spec.args = vec![serde_json::to_string(&snapshot)?];

        let _ = colonyos::submit(&fn_spec, &prvkey).await?;

        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
