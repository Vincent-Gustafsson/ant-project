use std::{collections::HashMap, io::Read};

use anyhow::{Context, Ok, Result};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bytes::Bytes;
use clap::Args;
use colonyos::core::{Blueprint, BlueprintMetadata, APPROVED};
use crypto_box::{PublicKey, SalsaBox, SecretKey, aead::{Aead, AeadCore, OsRng}};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tar::Archive;

use crate::config::AntConfig;
use crate::models::Deployment;

#[derive(Args)]
pub struct InstallArgs {
    /// The package to install (e.g. my-package@1.0.0)
    pub package: String,

    /// Optional values file to override defaults
    #[arg(short, long)]
    pub values: Option<std::path::PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServiceSpec {
    name: String,
    image: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    mounts: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ContainerSpec {
    name: String,
    image: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    replicas: u32,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    mounts: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StackSpec {
    name: String,
    network: Option<String>,
    #[serde(default)]
    volumes: Vec<String>,
    services: Vec<ServiceSpec>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Component {
    Container(ContainerSpec),
    Stack(StackSpec),
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

        if path.contains("/templates/") {
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

fn seal_env(env: &[String], recipient_pubkey: &PublicKey) -> Result<String> {
    let ephemeral_secret = SecretKey::generate(&mut OsRng);
    let ephemeral_public = ephemeral_secret.public_key();
    let salsa_box = SalsaBox::new(recipient_pubkey, &ephemeral_secret);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let plaintext = serde_json::to_vec(env)?;
    let ciphertext = salsa_box
        .encrypt(&nonce, plaintext.as_slice())
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;
    let mut sealed = Vec::with_capacity(32 + 24 + ciphertext.len());
    sealed.extend_from_slice(ephemeral_public.as_bytes());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    let b64 = B64.encode(&sealed);
    println!("Encrypted env ({} entries) → {} bytes sealed", env.len(), b64.len());
    anyhow::Ok(b64)
}

async fn get_controller_pubkey(colony_name: &str, prvkey: &str) -> Result<PublicKey> {
    let executors = colonyos::get_executors(colony_name, prvkey).await?;
    let controller = executors
        .into_iter()
        .find(|e| e.executortype == "deployment-controller" && e.state == APPROVED)
        .ok_or_else(|| anyhow::anyhow!("no approved deployment-controller found"))?;
    let pubkey_hex = controller
        .capabilities
        .software
        .iter()
        .find(|s| s.name == "pubkey")
        .map(|s| &s.software_type)
        .ok_or_else(|| anyhow::anyhow!("controller has no pubkey in software capabilities"))?;
    let bytes: [u8; 32] = hex::decode(pubkey_hex)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid controller pubkey length"))?;
    println!("Got controller pubkey: {pubkey_hex}");
    anyhow::Ok(PublicKey::from(bytes))
}

fn encrypt_env_in_value(val: &mut serde_json::Value, pubkey: &PublicKey) -> Result<()> {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(env_val) = map.get("env") {
                if let Some(env_arr) = env_val.as_array() {
                    let env: Vec<String> = env_arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    if !env.is_empty() {
                        let sealed = seal_env(&env, pubkey)?;
                        map.insert("env".to_string(), serde_json::Value::String(sealed));
                    }
                }
            }
            // recurse into "services" for Stack components
            if let Some(services) = map.get_mut("services") {
                if let Some(arr) = services.as_array_mut() {
                    for svc in arr {
                        encrypt_env_in_value(svc, pubkey)?;
                    }
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                encrypt_env_in_value(item, pubkey)?;
            }
        }
        _ => {}
    }
    anyhow::Ok(())
}

fn templates_to_blueprint(
    rendered: &HashMap<String, String>,
    pkg_name: &str,
    colony_name: &str,
    deployment_annotation: &str,
    controller_pubkey: &PublicKey,
) -> Result<Blueprint> {
    let mut components: Vec<Component> = Vec::new();

    for (file_name, content) in rendered {
        let component: Component = serde_yaml::from_str(content)
            .with_context(|| format!("Failed to parse template: {file_name}"))?;
        components.push(component);
    }

    let mut components_json =
        serde_json::to_value(&components).context("Failed to serialize components")?;
    encrypt_env_in_value(&mut components_json, controller_pubkey)?;

    let mut metadata = BlueprintMetadata::default();
    metadata.name = pkg_name.to_string();
    metadata.colonyname = colony_name.to_string();
    metadata
        .annotations
        .insert("deployment".to_string(), deployment_annotation.to_string());

    let spec = std::collections::HashMap::from([("components".to_string(), components_json)]);

    Ok(Blueprint {
        kind: "Deployment".to_string(),
        metadata,
        spec,
        ..Default::default()
    })
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

    let deployed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deployment_annotation = serde_json::to_string(&Deployment {
        pkg_name: name.to_string(),
        pkg_version: version.to_string(),
        revision: 0,
        deployed_at,
    })?;

    let controller_pubkey = get_controller_pubkey(&cfg.colony_name, &cfg.private_key).await?;
    let bp = templates_to_blueprint(
        &rendered,
        name,
        &cfg.colony_name,
        &deployment_annotation,
        &controller_pubkey,
    )?;
    colonyos::add_blueprint(&bp, &cfg.private_key).await?;

    println!("Success.");

    Ok(())
}
