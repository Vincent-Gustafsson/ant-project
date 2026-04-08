use std::collections::HashMap;

use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bollard::{
    Docker,
    models::{NetworkCreateRequest, VolumeCreateRequest},
    plugin::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder,
        RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use crypto_box::{PublicKey, SalsaBox, SecretKey, aead::Aead};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerStatus {
    pub container_id: Option<String>,
    pub node_name: String,
}

pub fn unseal_env(sealed_b64: &str, secret_key: &SecretKey) -> Result<Vec<String>> {
    println!("Decrypting env (sealed len={})", sealed_b64.len());
    let sealed = B64.decode(sealed_b64)?;
    if sealed.len() < 56 {
        anyhow::bail!("sealed env too short ({} bytes)", sealed.len());
    }
    let ephemeral_bytes: [u8; 32] = sealed[..32]
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid ephemeral pubkey"))?;
    let ephemeral_pubkey = PublicKey::from(ephemeral_bytes);
    let nonce_bytes: [u8; 24] = sealed[32..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid nonce"))?;
    let nonce = crypto_box::Nonce::from(nonce_bytes);
    let ciphertext = &sealed[56..];
    let salsa_box = SalsaBox::new(&ephemeral_pubkey, secret_key);
    let plaintext = salsa_box
        .decrypt(&nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed: {e}"))?;
    let env: Vec<String> = serde_json::from_slice(&plaintext)?;
    println!("Decrypted env: {} entries", env.len());
    Ok(env)
}

pub async fn spawn(
    kwargs: HashMap<String, Value>,
    secret_key: &SecretKey,
    docker: &Docker,
) -> Result<Vec<String>> {
    println!("spawn()");
    let image = kwargs
        .get("image")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("spawn requires an 'image' kwarg"))?
        .to_string();

    let env_b64 = kwargs
        .get("env")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let env = if env_b64.is_empty() {
        vec![]
    } else {
        unseal_env(env_b64, secret_key)?
    };
    println!("spawn env: {env:?}");

    let opts = CreateImageOptionsBuilder::default()
        .from_image(&image)
        .build();
    docker
        .create_image(Some(opts), None, None)
        .try_for_each(|_| async { Ok(()) })
        .await?;

    let opts = CreateContainerOptionsBuilder::default().build();
    let mut labels = HashMap::new();
    labels.insert("ant".to_string(), "true".to_string());

    let ctr = docker
        .create_container(
            Some(opts),
            ContainerCreateBody {
                image: Some(image.to_string()),
                labels: Some(labels),
                ..Default::default()
            },
        )
        .await?;

    docker.start_container(&ctr.id, None).await?;

    println!("Started container {} from {}", &ctr.id[..6], image);

    Ok(vec![ctr.id])
}

pub async fn kill(kwargs: HashMap<String, Value>, docker: &Docker) -> Result<Vec<String>> {
    println!("kill()");
    let ctr_id = kwargs
        .get("container")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("spawn requires a 'container' kwarg"))?
        .to_string();

    let opts = StopContainerOptionsBuilder::default().build();
    docker.stop_container(&ctr_id, Some(opts)).await?;

    println!("Stopped container from {}", &ctr_id[..6]);

    let opts = RemoveContainerOptionsBuilder::default().force(true).build();
    docker.remove_container(&ctr_id, Some(opts)).await?;

    println!("Removed container {}", &ctr_id[..6]);

    Ok(vec![])
}

pub async fn list(kwargs: HashMap<String, Value>, docker: &Docker) -> Result<Vec<String>> {
    println!("list()");
    let bp_name = kwargs
        .get("bp_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("spawn requires a 'bp_name' kwarg"))?
        .to_string();

    let mut filters = HashMap::new();
    filters.insert(
        "label",
        vec!["managed=true".into(), format!("bp_name={bp_name}")],
    );
    let opts = ListContainersOptionsBuilder::default()
        .filters(&filters)
        .build();

    let containers = docker.list_containers(Some(opts)).await?;
    containers
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

pub async fn ensure(
    kwargs: HashMap<String, Value>,
    secret_key: &SecretKey,
    docker: &Docker,
) -> Result<Vec<String>> {
    println!("ensure()");

    let bp_name = kwargs
        .get("bp_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ensure requires a 'bp_name' kwarg"))?
        .to_string();

    let component_name = kwargs
        .get("component_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ensure requires a 'component_name' kwarg"))?
        .to_string();

    let image = kwargs
        .get("image")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ensure requires an 'image' kwarg"))?
        .to_string();

    let desired: Vec<ContainerStatus> = kwargs
        .get("containers")
        .ok_or_else(|| anyhow::anyhow!("ensure requires a 'containers' kwarg"))
        .and_then(|v| serde_json::from_value(v.clone()).map_err(anyhow::Error::from))?;

    // list existing containers for this blueprint component on this node
    let mut filters = HashMap::new();
    filters.insert(
        "label",
        vec![
            "managed=true".into(),
            format!("bp_name={bp_name}"),
            format!("component_name={component_name}"),
        ],
    );
    let opts = ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filters)
        .build();
    let existing = docker.list_containers(Some(opts)).await?;

    let desired_ids: std::collections::HashSet<String> = desired
        .iter()
        .filter_map(|c| c.container_id.clone())
        .collect();

    // kill unlisted containers in parallel
    let kill_futures = existing.iter().filter_map(|ctr| {
        let id = ctr.id.clone()?;
        if desired_ids.contains(&id) {
            return None;
        }
        println!("ensure: removing unlisted container {}", &id[..6]);
        Some(async move {
            let opts = StopContainerOptionsBuilder::default().build();
            let _ = docker.stop_container(&id, Some(opts)).await;
            let opts = RemoveContainerOptionsBuilder::default().force(true).build();
            let _ = docker.remove_container(&id, Some(opts)).await;
        })
    });
    futures_util::future::join_all(kill_futures).await;

    let existing_ids: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|c| c.id.clone())
        .collect();

    let ports: Vec<String> = kwargs
        .get("ports")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let env: Vec<String> = match kwargs.get("env").and_then(|v| v.as_str()) {
        Some(sealed) if !sealed.is_empty() => unseal_env(sealed, secret_key)?,
        _ => vec![],
    };
    let mounts: Vec<String> = kwargs
        .get("mounts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // process all desired entries concurrently
    let entry_futures = desired.into_iter().map(|entry| {
        let image = image.clone();
        let bp_name = bp_name.clone();
        let component_name = component_name.clone();
        let ports = ports.clone();
        let env = env.clone();
        let mounts = mounts.clone();
        let existing_ids = existing_ids.clone();
        async move {
            match entry.container_id.as_deref() {
                Some(id) if existing_ids.contains(id) => {
                    // container exists — check if running, restart if not
                    let info = docker.inspect_container(id, None).await?;
                    let running = info
                        .state
                        .as_ref()
                        .and_then(|s| s.running)
                        .unwrap_or(false);
                    if !running {
                        println!("ensure: container {} not running, starting", &id[..6]);
                        docker.start_container(id, None).await?;
                    }
                    Ok::<ContainerStatus, anyhow::Error>(entry)
                }
                Some(id) => {
                    println!("ensure: container {} missing, re-creating", &id[..6]);
                    let new_id =
                        create_container(docker, &image, &bp_name, &component_name, &ports, &env, &mounts).await?;
                    Ok(ContainerStatus {
                        container_id: Some(new_id),
                        node_name: entry.node_name,
                    })
                }
                None => {
                    println!("ensure: creating new container");
                    let new_id =
                        create_container(docker, &image, &bp_name, &component_name, &ports, &env, &mounts).await?;
                    Ok(ContainerStatus {
                        container_id: Some(new_id),
                        node_name: entry.node_name,
                    })
                }
            }
        }
    });
    let result: Vec<ContainerStatus> = futures_util::future::try_join_all(entry_futures).await?;

    let json = serde_json::to_string(&result)?;
    Ok(vec![json])
}

async fn create_container(
    docker: &Docker,
    image: &str,
    bp_name: &str,
    component_name: &str,
    ports: &[String],
    env: &[String],
    mounts: &[String],
) -> Result<String> {
    let opts = CreateImageOptionsBuilder::default()
        .from_image(image)
        .build();
    docker
        .create_image(Some(opts), None, None)
        .try_for_each(|_| async { Ok(()) })
        .await?;

    let opts = CreateContainerOptionsBuilder::default().build();
    let mut labels = HashMap::new();
    labels.insert("ant".to_string(), "true".to_string());
    labels.insert("managed".to_string(), "true".to_string());
    labels.insert("bp_name".to_string(), bp_name.to_string());
    labels.insert("component_name".to_string(), component_name.to_string());

    let port_bindings = parse_port_bindings(ports);

    let host_config = bollard::plugin::HostConfig {
        port_bindings: if port_bindings.is_empty() {
            None
        } else {
            Some(port_bindings)
        },
        binds: if mounts.is_empty() {
            None
        } else {
            Some(mounts.to_vec())
        },
        ..Default::default()
    };

    let ctr = docker
        .create_container(
            Some(opts),
            ContainerCreateBody {
                image: Some(image.to_string()),
                labels: Some(labels),
                env: if env.is_empty() {
                    None
                } else {
                    Some(env.to_vec())
                },
                host_config: Some(host_config),
                ..Default::default()
            },
        )
        .await?;

    docker.start_container(&ctr.id, None).await?;
    println!("Started container {} from {}", &ctr.id[..6], image);
    Ok(ctr.id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceContainerStatus {
    pub name: String,
    pub container_id: String,
}

#[derive(Debug, Deserialize)]
struct ServiceSpec {
    name: String,
    image: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    mounts: Vec<String>,
}

pub async fn ensure_stack(
    kwargs: HashMap<String, Value>,
    secret_key: &SecretKey,
    docker: &Docker,
) -> Result<Vec<String>> {
    println!("ensure_stack()");

    let bp_name = kwargs
        .get("bp_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ensure_stack requires 'bp_name'"))?
        .to_string();

    let network: Option<String> = kwargs
        .get("network")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .flatten();

    let volumes: Vec<String> = kwargs
        .get("volumes")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let desired: Vec<ServiceSpec> = {
        let raw_services: Vec<Value> = kwargs
            .get("services")
            .ok_or_else(|| anyhow::anyhow!("ensure_stack requires 'services'"))
            .and_then(|v| serde_json::from_value(v.clone()).map_err(anyhow::Error::from))?;
        let mut specs = Vec::new();
        for mut svc in raw_services {
            // decrypt env if it's a sealed string
            if let Some(env_val) = svc.get("env") {
                if let Some(sealed) = env_val.as_str() {
                    if !sealed.is_empty() {
                        let decrypted = unseal_env(sealed, secret_key)?;
                        svc.as_object_mut().unwrap().insert(
                            "env".to_string(),
                            serde_json::to_value(&decrypted)?,
                        );
                    }
                }
            }
            specs.push(serde_json::from_value(svc)?);
        }
        specs
    };

    let existing: Vec<ServiceContainerStatus> = kwargs
        .get("existing")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // ensure network exists
    if let Some(ref net) = network {
        if docker.inspect_network(net, None).await.is_err() {
            println!("ensure_docker: creating network {net}");
            docker
                .create_network(NetworkCreateRequest {
                    name: net.clone(),
                    ..Default::default()
                })
                .await?;
        }
    }

    // ensure named volumes exist
    for vol in &volumes {
        let _ = docker
            .create_volume(VolumeCreateRequest {
                name: Some(vol.clone()),
                ..Default::default()
            })
            .await;
    }

    // build lookup of existing containers by name
    let existing_by_name: HashMap<String, String> = existing
        .into_iter()
        .map(|c| (c.name, c.container_id))
        .collect();

    let desired_names: std::collections::HashSet<&str> =
        desired.iter().map(|c| c.name.as_str()).collect();

    // kill containers for names no longer in the spec
    for (name, id) in &existing_by_name {
        if !desired_names.contains(name.as_str()) {
            println!("ensure_stack: removing dropped container {name} ({})", &id[..6]);
            let opts = StopContainerOptionsBuilder::default().build();
            let _ = docker.stop_container(id, Some(opts)).await;
            let opts = RemoveContainerOptionsBuilder::default().force(true).build();
            let _ = docker.remove_container(id, Some(opts)).await;
        }
    }

    // process each desired container concurrently
    let futures = desired.into_iter().map(|spec| {
        let bp_name = bp_name.clone();
        let network = network.clone();
        let existing_id = existing_by_name.get(&spec.name).cloned();
        async move {
            let container_id =
                ensure_stack_container(docker, &bp_name, &spec, network.as_deref(), existing_id)
                    .await?;
            Ok::<ServiceContainerStatus, anyhow::Error>(ServiceContainerStatus {
                name: spec.name,
                container_id,
            })
        }
    });

    let updated: Vec<ServiceContainerStatus> =
        futures_util::future::try_join_all(futures).await?;

    let json = serde_json::to_string(&updated)?;
    Ok(vec![json])
}

async fn ensure_stack_container(
    docker: &Docker,
    bp_name: &str,
    spec: &ServiceSpec,
    network: Option<&str>,
    existing_id: Option<String>,
) -> Result<String> {
    if let Some(ref id) = existing_id {
        match docker.inspect_container(id, None).await {
            Ok(info) => {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                if !running {
                    println!("ensure_stack: starting stopped container {} ({})", spec.name, &id[..6]);
                    docker.start_container(id, None).await?;
                }
                return Ok(id.clone());
            }
            Err(_) => {
                println!("ensure_stack: container {} gone, re-creating", spec.name);
            }
        }
    }

    println!("ensure_stack: creating container {}", spec.name);

    let opts = CreateImageOptionsBuilder::default()
        .from_image(&spec.image)
        .build();
    docker
        .create_image(Some(opts), None, None)
        .try_for_each(|_| async { Ok(()) })
        .await?;

    let mut labels = HashMap::new();
    labels.insert("ant".to_string(), "true".to_string());
    labels.insert("managed".to_string(), "true".to_string());
    labels.insert("bp_name".to_string(), bp_name.to_string());
    labels.insert("container_name".to_string(), spec.name.clone());

    let port_bindings = parse_port_bindings(&spec.ports);
    let host_config = bollard::plugin::HostConfig {
        port_bindings: if port_bindings.is_empty() { None } else { Some(port_bindings) },
        binds: if spec.mounts.is_empty() { None } else { Some(spec.mounts.clone()) },
        ..Default::default()
    };

    let networking_config = network.map(|net| bollard::models::NetworkingConfig {
        endpoints_config: Some(HashMap::from([(
            net.to_string(),
            bollard::models::EndpointSettings::default(),
        )])),
    });

    let opts = CreateContainerOptionsBuilder::default()
        .name(&spec.name)
        .build();
    let ctr = docker
        .create_container(
            Some(opts),
            ContainerCreateBody {
                image: Some(spec.image.clone()),
                cmd: if spec.command.is_empty() { None } else { Some(spec.command.clone()) },
                labels: Some(labels),
                env: if spec.env.is_empty() { None } else { Some(spec.env.clone()) },
                host_config: Some(host_config),
                networking_config,
                ..Default::default()
            },
        )
        .await?;

    docker.start_container(&ctr.id, None).await?;
    println!("ensure_stack: started {} ({})", spec.name, &ctr.id[..6]);
    Ok(ctr.id)
}

/// Parse port specs like "8080:8080" into bollard PortBinding map.
fn parse_port_bindings(
    ports: &[String],
) -> HashMap<String, Option<Vec<bollard::plugin::PortBinding>>> {
    let mut map = HashMap::new();
    for port in ports {
        let parts: Vec<&str> = port.splitn(2, ':').collect();
        if parts.len() == 2 {
            let host_port = parts[0].to_string();
            let container_port = format!("{}/tcp", parts[1]);
            map.insert(
                container_port,
                Some(vec![bollard::plugin::PortBinding {
                    host_ip: None,
                    host_port: Some(host_port),
                }]),
            );
        }
    }
    map
}
