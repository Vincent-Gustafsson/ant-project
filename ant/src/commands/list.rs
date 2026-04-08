use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use clap::Args;

use crate::config::AntConfig;
use crate::models::Deployment;

fn format_deployed_at(deployed_at: &str) -> String {
    let Ok(dt) = deployed_at.parse::<DateTime<Utc>>() else {
        return deployed_at.to_string();
    };
    let local = dt.with_timezone(&Local);
    let today = Utc::now().with_timezone(&Local).date_naive();
    if local.date_naive() == today {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m-%d %H:%M").to_string()
    }
}

#[derive(Args)]
pub struct ListArgs {}

pub async fn run(_args: ListArgs) -> Result<()> {
    let cfg = AntConfig::load()?;
    colonyos::set_server_url(&cfg.colony_server_url);

    let deployments: Vec<Deployment> =
        colonyos::get_blueprints(&cfg.colony_name, "", "", &cfg.private_key)
            .await?
            .into_iter()
            .filter_map(|bp| {
                let raw = bp.metadata.annotations.get("deployment")?;
                serde_json::from_str(raw).ok()
            })
            .collect();

    println!("{:<20} {:<12} {}", "NAME", "VERSION", "DEPLOYED AT");
    for d in &deployments {
        println!("{:<20} {:<12} {}", d.pkg_name, d.pkg_version, format_deployed_at(&d.deployed_at));
    }

    Ok(())
}
