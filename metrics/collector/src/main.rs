mod executor;
use anyhow::Result;

use crate::executor::run_executor;

#[tokio::main]
async fn main() -> Result<()> {
    colonyos::set_server_url("https://colony.colonypm.xyz:443/api");
    run_executor().await
}
