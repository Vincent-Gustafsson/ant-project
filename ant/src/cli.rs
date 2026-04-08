use crate::commands::{init, install, list, remove};
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "Amt",
    version,
    about = "A package manager for ColonyOS",
    long_about = None,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new project
    Init(init::InitArgs),
    /// Install a package
    Install(install::InstallArgs),
    /// List deployed packages
    List(list::ListArgs),
    /// Remove a deployed package
    Remove(remove::RemoveArgs),
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Commands::Init(args) => init::run(args),
            Commands::Install(args) => install::run(args).await,
            Commands::List(args) => list::run(args).await,
            Commands::Remove(args) => remove::run(args).await,
        }
    }
}
