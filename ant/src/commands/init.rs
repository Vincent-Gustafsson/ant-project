use std::{env::current_dir, fs};

use anyhow::Result;
use clap::Args;
use git2::Repository;
use serde::Serialize;

#[derive(Serialize)]
struct PackageManifest {
    name: String,
    version: String,
    description: String,
}

const WORKFLOWS_DIR: &str = ".github/workflows";

fn publish_workflow(package_name: &str) -> String {
    format!(
        "\
name: Publish
on:
  push:
    tags:
      - 'v*'
jobs:
  publish:
    runs-on: ubuntu-latest
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: actions/checkout@v4
      - name: Create archive
        run: |
          mkdir /tmp/{package_name}
          cp README.md values.yaml package.yaml /tmp/{package_name}/
          cp -r templates /tmp/{package_name}/templates
          tar -czf /tmp/archive.tar.gz -C /tmp {package_name}
      - name: Publish
        uses: ant-pm/publish-action@v1
        with:
          registry_url: https://ant.vinlaro.com/api/packages
"
    )
}

#[derive(Args)]
pub struct InitArgs {}

pub fn run(_args: InitArgs) -> Result<()> {
    // 1. Ensure package directory is empty
    let cwd = current_dir()?;
    let is_dir_empty = cwd.read_dir()?.next().is_none();

    if !is_dir_empty {
        anyhow::bail!("directory is not empty");
    }

    // 2. Initialize git repository
    let _ = Repository::init(".")?;

    // 3. Create package.yaml
    let manifest = PackageManifest {
        name: cwd
            .file_name()
            .expect("cwd should have a valid name")
            .to_string_lossy()
            .into_owned(),
        version: "0.1.0".to_string(),
        description: "My new package".to_string(),
    };

    let yaml = serde_yaml::to_string(&manifest)?;
    fs::write("package.yaml", yaml)?;

    // 4. Create README
    fs::write(
        "README.md",
        format!("# {}\n\nA new package.\n", manifest.name),
    )?;

    // 5. Create templates directory
    fs::create_dir("templates")?;

    // 6. Create a values.yaml file
    fs::write("values.yaml", "")?;

    // 7. Create GitHub Actions workflow
    fs::create_dir_all(WORKFLOWS_DIR)?;
    fs::write(
        format!("{WORKFLOWS_DIR}/publish.yaml"),
        publish_workflow(&manifest.name),
    )?;

    Ok(())
}
