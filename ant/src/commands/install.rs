use std::{collections::HashMap, io::Read};

use anyhow::{Context, Ok, Result};
use bytes::Bytes;
use clap::Args;
use colonyos::core::Blueprint;
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tar::Archive;

use crate::config::AntConfig;

#[derive(Args)]
pub struct InstallArgs {
    /// The package to install (e.g. my-package@1.0.0)
    pub package: String,

    /// Optional values file to override defaults
    #[arg(short, long)]
    pub values: Option<std::path::PathBuf>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Deployment {
    pkg_name: String,
    pkg_version: String,
    revision: i32,
}

async fn get_download_url(client: &Client, name: &str, version: &str) -> Result<String> {
    let json = client
        .get(format!(
            "https://ant.vinlaro.com/api/packages/{name}/{version}/download"
        ))
        .send()
        .await
        .context("Failed to send request")?
        .error_for_status()
        .context("Server returned error")?
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse response")?;

    let url = json
        .get("download_url")
        .and_then(|v| v.as_str())
        .context("Missing or invalid download_url")?
        .to_owned();

    Ok(url)
}

async fn download_package_archive(client: &Client, url: &str) -> Result<Bytes> {
    client
        .get(url)
        .send()
        .await
        .context("Failed to download")?
        .error_for_status()
        .context("Server error")?
        .bytes()
        .await
        .context("Failed to read bytes")
}

fn process_archive(archive_bytes: &[u8]) -> Result<(HashMap<String, String>, serde_yaml::Value)> {
    let mut archive = Archive::new(GzDecoder::new(archive_bytes));
    let mut templates: HashMap<String, String> = HashMap::new();
    let mut values: Option<serde_yaml::Value> = None;

    for entry in archive.entries().context("Failed to read archive")? {
        let mut entry = entry.context("Failed to read entry")?;
        let path = entry.path()?.to_string_lossy().to_string();

        let Some(file_name) = entry
            .path()?
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
        else {
            continue;
        };

        if path.ends_with('/') {
            continue;
        }

        let mut contents = String::new();
        entry.read_to_string(&mut contents)?;

        if path.starts_with("./templates/") {
            templates.insert(file_name, contents);
        } else if file_name == "values.yaml" {
            if !contents.trim().is_empty() {
                values = Some(serde_yaml::from_str(&contents)?);
            }
        }
    }

    let values = values.unwrap_or(serde_yaml::Value::Mapping(Default::default()));

    Ok((templates, values))
}

fn merge_values(base: &mut serde_yaml::Value, overrides: serde_yaml::Value) {
    match (base, overrides) {
        (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(overrides)) => {
            for (k, v) in overrides {
                merge_values(base.entry(k).or_insert(serde_yaml::Value::Null), v);
            }
        }
        (base, overrides) => *base = overrides,
    }
}

fn apply_value_overrides(
    values: &mut serde_yaml::Value,
    values_path: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(values_path) = values_path {
        let contents =
            std::fs::read_to_string(&values_path).context("Failed to read values file")?;

        let user_values: serde_yaml::Value =
            serde_yaml::from_str(&contents).context("Failed to parse values file")?;

        merge_values(values, user_values);
    }
    Ok(())
}

fn render_templates(
    templates: &HashMap<String, String>,
    values: &serde_yaml::Value,
) -> Result<HashMap<String, String>> {
    let mut tera = tera::Tera::default();
    tera.add_raw_templates(templates.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .context("Failed to add templates")?;

    let context =
        tera::Context::from_serialize(values).context("Failed to build context from values")?;

    templates
        .keys()
        .map(|name| {
            let rendered = tera
                .render(name, &context)
                .with_context(|| format!("Failed to render template: {name}"))?;
            Ok((name.clone(), rendered))
        })
        .collect()
}

pub async fn run(args: InstallArgs) -> Result<()> {
    let cfg = AntConfig::load()?;
    colonyos::set_server_url(&cfg.colony_server_url);

    let (name, version) = args
        .package
        .split_once('@')
        .context("Invalid format, expected name@version (e.g. fibbers@1.0.0)")?;

    // Deployment already exists?
    let already_exists = colonyos::get_blueprints(&cfg.colony_name, "", "", &cfg.private_key)
        .await?
        .into_iter()
        .filter_map(|bp| {
            let raw = bp.metadata.annotations.get("deployment")?;
            serde_json::from_str::<Deployment>(raw).ok()
        })
        .any(|d| d.pkg_name == name);

    if already_exists {
        anyhow::bail!("{} is already deployed", args.package);
    }

    // Download the package tarball
    let client = Client::new();
    let download_url = get_download_url(&client, name, version).await?;
    let archive_bytes = download_package_archive(&client, &download_url).await?;

    // Extract and render templates + values
    let (templates, mut values) = process_archive(&archive_bytes)?;
    apply_value_overrides(&mut values, args.values)?;
    let rendered = render_templates(&templates, &values)?;

    // Add blueprints to the server
    let blueprints: Vec<Blueprint> = rendered
        .values()
        .map(|s| serde_yaml::from_str(s))
        .collect::<Result<Vec<_>, _>>()?;

    for mut bp in blueprints {
        bp.metadata.annotations.insert(
            "deployment".to_string(),
            serde_json::to_string(&Deployment {
                pkg_name: name.to_string(),
                pkg_version: version.to_string(),
                revision: 0,
            })?,
        );

        let _ = colonyos::add_blueprint(&bp, &cfg.private_key).await?;
    }

    println!("Success.");

    Ok(())
}
