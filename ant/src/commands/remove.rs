use anyhow::{Context, Result};
use clap::Args;
use colonyos::core::{FunctionSpec, SUCCESS};

use crate::config::AntConfig;
use crate::models::Deployment;

#[derive(Args)]
pub struct RemoveArgs {
    /// The package to remove (e.g. my-package)
    pub package: String,
}

pub async fn run(args: RemoveArgs) -> Result<()> {
    let cfg = AntConfig::load()?;
    colonyos::set_server_url(&cfg.colony_server_url);

    let exists = colonyos::get_blueprints(&cfg.colony_name, "", "", &cfg.private_key)
        .await?
        .into_iter()
        .filter_map(|bp| {
            let raw = bp.metadata.annotations.get("deployment")?;
            serde_json::from_str::<Deployment>(raw).ok()
        })
        .any(|d| d.pkg_name == args.package);

    if !exists {
        anyhow::bail!("{} is not deployed", args.package);
    }

    println!("Cleaning up {}...", args.package);

    let mut fn_spec = FunctionSpec::new("cleanup", "deployment-controller", &cfg.colony_name);
    fn_spec.kwargs.insert(
        "blueprintName".to_string(),
        serde_json::Value::String(args.package.clone()),
    );
    fn_spec.maxexectime = -1;

    let proc = colonyos::submit(&fn_spec, &cfg.private_key)
        .await
        .context("Failed to submit cleanup process")?;
    colonyos::subscribe_process(&proc, SUCCESS, 120, &cfg.private_key)
        .await
        .context("Cleanup process failed")?;

    colonyos::remove_blueprint(&cfg.colony_name, &args.package, &cfg.private_key)
        .await
        .context("Failed to remove blueprint")?;

    println!("Removed {}.", args.package);

    Ok(())
}
