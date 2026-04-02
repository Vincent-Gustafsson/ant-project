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

    // Prime the CPU delta
    sys.refresh_cpu_all();
    sleep(Duration::from_millis(200)).await;

    loop {
        println!(
            "{} Collecting metrics...",
            Local::now().format("[%H:%M:%S]")
        );

        let snapshot = collect(&mut sys, &mut disks);

        let mut fn_spec = FunctionSpec::new("push_metrics", "metrics-collector", "dev");
        fn_spec.args = vec![serde_json::to_string(&snapshot)?];

        let _ = colonyos::submit(
            &fn_spec,
            "ddf7f7791208083b6a9ed975a72684f6406a269cfa36f1b1c32045c0a71fff05",
        )
        .await?;

        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
