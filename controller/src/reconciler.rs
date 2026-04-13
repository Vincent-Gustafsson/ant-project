use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use colonyos::core::{APPROVED, Blueprint, Executor, FunctionSpec, Process, SUCCESS};
use crypto_box::{PublicKey, SalsaBox, SecretKey, aead::{Aead, AeadCore, OsRng}};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::executor::{encryption_key, exec_prvkey};

// --- Specs ---

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
    env: Value,
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
    env: Value,
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

// --- Status types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContainerStatus {
    container_id: Option<String>,
    node_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceContainerStatus {
    name: String,
    container_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StackStatus {
    node_name: String,
    containers: Vec<ServiceContainerStatus>,
}

// --- Encryption helpers ---

fn unseal_env(sealed_b64: &str, secret_key: &SecretKey) -> Result<Vec<String>> {
    let sealed = B64.decode(sealed_b64)?;
    if sealed.len() < 56 {
        anyhow::bail!("sealed env too short ({} bytes)", sealed.len());
    }
    let ephemeral_bytes: [u8; 32] = sealed[..32]
        .try_into()
        .map_err(|_| anyhow!("invalid ephemeral pubkey"))?;
    let ephemeral_pubkey = PublicKey::from(ephemeral_bytes);
    let nonce_bytes: [u8; 24] = sealed[32..56]
        .try_into()
        .map_err(|_| anyhow!("invalid nonce"))?;
    let nonce = crypto_box::Nonce::from(nonce_bytes);
    let ciphertext = &sealed[56..];
    let salsa_box = SalsaBox::new(&ephemeral_pubkey, secret_key);
    let plaintext = salsa_box
        .decrypt(&nonce, ciphertext)
        .map_err(|e| anyhow!("decryption failed: {e}"))?;
    let env: Vec<String> = serde_json::from_slice(&plaintext)?;
    Ok(env)
}

fn seal_env(env: &[String], recipient_pubkey: &PublicKey) -> Result<String> {
    let ephemeral_secret = SecretKey::generate(&mut OsRng);
    let ephemeral_public = ephemeral_secret.public_key();
    let salsa_box = SalsaBox::new(recipient_pubkey, &ephemeral_secret);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let plaintext = serde_json::to_vec(env)?;
    let ciphertext = salsa_box
        .encrypt(&nonce, plaintext.as_slice())
        .map_err(|e| anyhow!("encryption failed: {e}"))?;
    let mut sealed = Vec::with_capacity(32 + 24 + ciphertext.len());
    sealed.extend_from_slice(ephemeral_public.as_bytes());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(B64.encode(&sealed))
}

fn get_node_pubkey(node: &Executor) -> Result<PublicKey> {
    let pubkey_hex = node
        .capabilities
        .software
        .iter()
        .find(|s| s.name == "pubkey")
        .map(|s| &s.software_type)
        .ok_or_else(|| anyhow!("node {} has no pubkey", node.executorname))?;
    let bytes: [u8; 32] = hex::decode(pubkey_hex)?
        .try_into()
        .map_err(|_| anyhow!("invalid pubkey length for {}", node.executorname))?;
    Ok(PublicKey::from(bytes))
}

/// Re-encrypt env: decrypt with controller key, re-encrypt for target node
fn reencrypt_env(sealed_b64: &str, node: &Executor) -> Result<String> {
    let controller_key = encryption_key();
    let env = unseal_env(sealed_b64, controller_key)?;
    let node_pubkey = get_node_pubkey(node)?;
    seal_env(&env, &node_pubkey)
}

// --- Scheduler traits ---

trait ContainerScheduler {
    fn scale_up(
        &self,
        nodes: &[Executor],
        existing: &[ContainerStatus],
        count: u32,
    ) -> Result<Vec<String>>;
    fn scale_down(&self, existing: Vec<ContainerStatus>, keep: u32) -> Vec<ContainerStatus>;
}

trait StackScheduler {
    fn pick_node(&self, nodes: &[Executor]) -> Result<String>;
}

// --- Concrete schedulers ---

struct SpreadScheduler;
struct StickyScheduler;

impl ContainerScheduler for SpreadScheduler {
    fn scale_up(
        &self,
        nodes: &[Executor],
        existing: &[ContainerStatus],
        count: u32,
    ) -> Result<Vec<String>> {
        let mut node_counts: HashMap<String, u32> =
            nodes.iter().map(|n| (n.executorname.clone(), 0u32)).collect();
        for c in existing {
            *node_counts.entry(c.node_name.clone()).or_default() += 1;
        }

        let mut result = vec![];
        for _ in 0..count {
            let target = node_counts
                .iter_mut()
                .min_by_key(|(_, c)| **c)
                .map(|(name, count)| {
                    *count += 1;
                    name.clone()
                })
                .ok_or_else(|| anyhow!("no nodes available"))?;
            result.push(target);
        }
        Ok(result)
    }

    fn scale_down(&self, mut existing: Vec<ContainerStatus>, keep: u32) -> Vec<ContainerStatus> {
        let remove = existing.len() as u32 - keep;
        let mut node_counts: HashMap<String, u32> = HashMap::new();
        for c in &existing {
            *node_counts.entry(c.node_name.clone()).or_default() += 1;
        }
        for _ in 0..remove {
            let target = node_counts
                .iter()
                .max_by_key(|(_, c)| *c)
                .map(|(n, _)| n.clone())
                .unwrap();
            let pos = existing.iter().rposition(|c| c.node_name == target).unwrap();
            existing.remove(pos);
            *node_counts.get_mut(&target).unwrap() -= 1;
        }
        existing
    }
}

impl StackScheduler for StickyScheduler {
    fn pick_node(&self, nodes: &[Executor]) -> Result<String> {
        nodes
            .first()
            .map(|n| n.executorname.clone())
            .ok_or_else(|| anyhow!("no nodes available"))
    }
}

// --- Status helpers ---

fn read_container_status(bp: &Blueprint, name: &str) -> Result<Vec<ContainerStatus>> {
    let val = bp.status.get(name).cloned().unwrap_or(Value::Array(vec![]));
    serde_json::from_value(val).context("failed to deserialize container status")
}

fn read_stack_status(bp: &Blueprint, name: &str) -> Result<Option<StackStatus>> {
    let Some(val) = bp.status.get(name).cloned() else {
        return Ok(None);
    };
    serde_json::from_value(val)
        .context("failed to deserialize stack status")
        .map(Some)
}

// Writes a single component's status, using current_status as the base so that
// concurrent component writes within the same reconcile pass don't overwrite
// each other's updates.
async fn write_component_status(
    bp_name: &str,
    current_status: &mut HashMap<String, Value>,
    name: &str,
    value: Value,
) -> Result<()> {
    current_status.insert(name.to_string(), value);
    colonyos::update_blueprint_status("dev", bp_name, current_status.clone(), exec_prvkey())
        .await
        .context("failed to update blueprint status")
}

// --- Helpers ---

async fn approved_nodes() -> Result<Vec<Executor>> {
    let nodes = colonyos::get_executors("dev", exec_prvkey())
        .await?
        .into_iter()
        .filter(|e| e.executortype == "node-agent" && e.state == APPROVED)
        .collect();
    Ok(nodes)
}

fn reencrypt_env_for_node(env: &Value, node: &Executor) -> Result<Value> {
    match env.as_str() {
        Some(sealed) if !sealed.is_empty() => {
            let resealed = reencrypt_env(sealed, node)?;
            Ok(Value::String(resealed))
        }
        _ => Ok(env.clone()),
    }
}

async fn ensure_container_nodes(
    bp: &Blueprint,
    spec: &ContainerSpec,
    containers: &[ContainerStatus],
    previous: &[ContainerStatus],
    nodes: &[Executor],
) -> Result<Vec<ContainerStatus>> {
    let mut by_node: HashMap<String, Vec<ContainerStatus>> = HashMap::new();
    for c in previous {
        by_node.entry(c.node_name.clone()).or_default();
    }
    for c in containers {
        by_node
            .entry(c.node_name.clone())
            .or_default()
            .push(c.clone());
    }

    let exec_prvkey = exec_prvkey();
    let mut futures_vec = Vec::new();
    for (node_name, node_containers) in by_node {
        let node = nodes.iter().find(|n| n.executorname == node_name);
        let env_value = match node {
            Some(n) => reencrypt_env_for_node(&spec.env, n)?,
            None => spec.env.clone(),
        };

        let mut fn_spec = FunctionSpec::new("ensure", "node-agent", "dev");
        fn_spec.conditions.executornames = vec![node_name.clone()];
        fn_spec.maxexectime = -1;
        fn_spec.kwargs = HashMap::from([
            (
                "bp_name".into(),
                serde_json::to_value(&bp.metadata.name).unwrap(),
            ),
            (
                "component_name".into(),
                serde_json::to_value(&spec.name).unwrap(),
            ),
            ("image".into(), serde_json::to_value(&spec.image).unwrap()),
            ("ports".into(), serde_json::to_value(&spec.ports).unwrap()),
            ("env".into(), env_value),
            ("mounts".into(), serde_json::to_value(&spec.mounts).unwrap()),
            (
                "containers".into(),
                serde_json::to_value(&node_containers).unwrap(),
            ),
        ]);

        futures_vec.push(async move {
            let proc = colonyos::submit(&fn_spec, exec_prvkey).await?;
            colonyos::subscribe_process(&proc, SUCCESS, 120, exec_prvkey).await?;
            let proc = colonyos::get_process(&proc.processid, exec_prvkey).await?;
            let s = proc
                .output
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("ensure returned no output on {node_name}"))?;
            serde_json::from_str::<Vec<ContainerStatus>>(&s)
                .context("failed to deserialize ensure response")
        });
    }

    Ok(try_join_all(futures_vec).await?.into_iter().flatten().collect())
}

// --- Component reconcilers ---

async fn reconcile_container(
    bp: &Blueprint,
    spec: &ContainerSpec,
    scheduler: &dyn ContainerScheduler,
    current_status: &mut HashMap<String, Value>,
) -> Result<()> {
    let previous = read_container_status(bp, &spec.name)?;
    let mut containers = previous.clone();
    let actual = containers.len() as u32;
    let desired = spec.replicas;

    println!(
        "[{}] container '{}': reconciling (image={} actual={} desired={})",
        bp.metadata.name, spec.name, spec.image, actual, desired
    );

    let nodes = approved_nodes().await?;

    if actual < desired {
        let node_names = scheduler.scale_up(&nodes, &containers, desired - actual)?;
        println!(
            "[{}] container '{}': scaling up {} → {} (nodes: {})",
            bp.metadata.name, spec.name, actual, desired,
            node_names.join(", ")
        );
        containers.extend(node_names.into_iter().map(|node_name| ContainerStatus {
            container_id: None,
            node_name,
        }));
        write_component_status(&bp.metadata.name, current_status, &spec.name, serde_json::to_value(&containers)?).await?;
    } else if actual > desired {
        println!(
            "[{}] container '{}': scaling down {} → {}",
            bp.metadata.name, spec.name, actual, desired
        );
        containers = scheduler.scale_down(containers, desired);
    }

    let updated = ensure_container_nodes(bp, spec, &containers, &previous, &nodes).await?;
    write_component_status(&bp.metadata.name, current_status, &spec.name, serde_json::to_value(&updated)?).await?;

    println!(
        "[{}] container '{}': OK ({}/{} replica(s))",
        bp.metadata.name, spec.name, updated.len(), desired
    );
    for (i, c) in updated.iter().enumerate() {
        println!(
            "[{}] container '{}':   replica {} → node={} id={}",
            bp.metadata.name, spec.name, i,
            c.node_name,
            c.container_id.as_deref().unwrap_or("pending"),
        );
    }
    Ok(())
}

async fn reconcile_stack(
    bp: &Blueprint,
    spec: &StackSpec,
    scheduler: &dyn StackScheduler,
    current_status: &mut HashMap<String, Value>,
) -> Result<()> {
    let current = read_stack_status(bp, &spec.name)?;
    let nodes = approved_nodes().await?;

    println!(
        "[{}] stack '{}': reconciling ({} service(s))",
        bp.metadata.name, spec.name, spec.services.len()
    );

    let (node_name, existing) = if let Some(ref s) = current {
        if nodes.iter().any(|n| n.executorname == s.node_name) {
            (s.node_name.clone(), s.containers.clone())
        } else {
            println!(
                "[{}] stack '{}': node {} unavailable, rescheduling",
                bp.metadata.name, spec.name, s.node_name
            );
            (scheduler.pick_node(&nodes)?, vec![])
        }
    } else {
        (scheduler.pick_node(&nodes)?, vec![])
    };

    // Re-encrypt service env fields for the target node
    let target_node = nodes.iter().find(|n| n.executorname == node_name);
    let mut services_json = serde_json::to_value(&spec.services)?;
    if let (Some(node), Some(arr)) = (target_node, services_json.as_array_mut()) {
        for svc in arr {
            if let Some(env_val) = svc.get("env") {
                if let Some(sealed) = env_val.as_str() {
                    if !sealed.is_empty() {
                        let resealed = reencrypt_env(sealed, node)?;
                        svc.as_object_mut().unwrap().insert(
                            "env".to_string(),
                            Value::String(resealed),
                        );
                    }
                }
            }
        }
    }

    let exec_prvkey = exec_prvkey();
    let mut fn_spec = FunctionSpec::new("ensure_stack", "node-agent", "dev");
    fn_spec.conditions.executornames = vec![node_name.clone()];
    fn_spec.maxexectime = 300;
    fn_spec.kwargs = HashMap::from([
        (
            "bp_name".into(),
            serde_json::to_value(&bp.metadata.name).unwrap(),
        ),
        (
            "component_name".into(),
            serde_json::to_value(&spec.name).unwrap(),
        ),
        (
            "network".into(),
            serde_json::to_value(&spec.network).unwrap(),
        ),
        (
            "volumes".into(),
            serde_json::to_value(&spec.volumes).unwrap(),
        ),
        ("services".into(), services_json),
        (
            "existing".into(),
            serde_json::to_value(&existing).unwrap(),
        ),
    ]);

    println!(
        "[{}] stack '{}': ensuring {} service(s) on {}",
        bp.metadata.name, spec.name, spec.services.len(), node_name
    );

    let proc = colonyos::submit(&fn_spec, exec_prvkey).await?;
    colonyos::subscribe_process(&proc, SUCCESS, 300, exec_prvkey).await?;
    let proc = colonyos::get_process(&proc.processid, exec_prvkey).await?;
    let s = proc
        .output
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("ensure_stack returned no output"))?;
    let updated: Vec<ServiceContainerStatus> =
        serde_json::from_str(&s).context("failed to deserialize ensure_stack response")?;

    let stack_status = StackStatus { node_name, containers: updated };
    write_component_status(&bp.metadata.name, current_status, &spec.name, serde_json::to_value(&stack_status)?).await?;

    println!(
        "[{}] stack '{}': OK ({} service(s) on {})",
        bp.metadata.name, spec.name, stack_status.containers.len(), stack_status.node_name
    );
    Ok(())
}

// --- Entry points ---

fn get_components(bp: &Blueprint) -> Result<Vec<Component>> {
    let components = bp
        .spec
        .get("components")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    serde_json::from_value(components)
        .context("failed to deserialize components from blueprint spec")
}

pub async fn reconcile(proc: &Process) -> Result<Vec<String>> {
    let name = proc
        .spec
        .kwargs
        .get("blueprintName")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let blueprints: Vec<Blueprint> = match name {
        Some(n) => {
            match colonyos::get_blueprint("dev", &n, exec_prvkey()).await {
                Ok(bp) => vec![bp],
                Err(_) => {
                    println!("RECONCILE {n}: blueprint not found, skipping");
                    return Ok(vec![]);
                }
            }
        }
        None => colonyos::get_blueprints("dev", "Deployment", "", exec_prvkey())
            .await
            .context("failed to fetch blueprints")?,
    };

    let container_scheduler = SpreadScheduler;
    let stack_scheduler = StickyScheduler;

    for bp in &blueprints {
        let components = get_components(bp)?;
        println!("RECONCILE {}: {} component(s)", bp.metadata.name, components.len());

        // current_status accumulates all writes within this reconcile pass so that
        // each component's write includes updates from previously-reconciled components.
        let mut current_status = bp.status.clone();

        for component in &components {
            let component_name = match component {
                Component::Container(s) => s.name.as_str(),
                Component::Stack(s) => s.name.as_str(),
            };

            let result = match component {
                Component::Container(spec) => {
                    reconcile_container(bp, spec, &container_scheduler, &mut current_status).await
                }
                Component::Stack(spec) => {
                    reconcile_stack(bp, spec, &stack_scheduler, &mut current_status).await
                }
            };

            if let Err(ref e) = result {
                println!("[{}] '{}': FAILED - {e}", bp.metadata.name, component_name);
                result?;
            }
        }
    }

    Ok(vec![])
}

pub async fn cleanup(proc: &Process) -> Result<Vec<String>> {
    let name = parse_bp_name(proc)?;
    println!("CLEANUP: {name}");

    let bp: Blueprint = match colonyos::get_blueprint("dev", &name, exec_prvkey()).await {
        Ok(bp) => bp,
        Err(_) => {
            println!("CLEANUP {name}: blueprint not found, skipping");
            return Ok(vec![]);
        }
    };

    let components = get_components(&bp)?;

    for component in &components {
        match component {
            Component::Container(spec) => cleanup_container(&bp, spec).await?,
            Component::Stack(spec) => cleanup_stack(&bp, spec).await?,
        }
    }

    Ok(vec![])
}

async fn cleanup_container(bp: &Blueprint, spec: &ContainerSpec) -> Result<()> {
    let containers = read_container_status(bp, &spec.name)?;
    if containers.is_empty() {
        return Ok(());
    }

    println!("[{}] container '{}': cleaning up", bp.metadata.name, spec.name);

    let nodes: std::collections::HashSet<String> =
        containers.iter().map(|c| c.node_name.clone()).collect();

    let exec_prvkey = exec_prvkey();
    let futures = nodes.into_iter().map(|node_name| {
        let mut fn_spec = FunctionSpec::new("ensure", "node-agent", "dev");
        fn_spec.conditions.executornames = vec![node_name.clone()];
        fn_spec.maxexectime = -1;
        fn_spec.kwargs = HashMap::from([
            (
                "bp_name".into(),
                serde_json::to_value(&bp.metadata.name).unwrap(),
            ),
            (
                "component_name".into(),
                serde_json::to_value(&spec.name).unwrap(),
            ),
            ("image".into(), serde_json::to_value(&spec.image).unwrap()),
            ("ports".into(), serde_json::to_value(&spec.ports).unwrap()),
            ("env".into(), Value::String(String::new())),
            ("mounts".into(), serde_json::to_value(&spec.mounts).unwrap()),
            (
                "containers".into(),
                serde_json::to_value(&[] as &[ContainerStatus]).unwrap(),
            ),
        ]);

        async move {
            let proc = colonyos::submit(&fn_spec, exec_prvkey).await?;
            colonyos::subscribe_process(&proc, SUCCESS, 120, exec_prvkey).await?;
            Ok::<(), anyhow::Error>(())
        }
    });

    try_join_all(futures).await?;
    println!("[{}] container '{}': cleaned up", bp.metadata.name, spec.name);
    Ok(())
}

async fn cleanup_stack(bp: &Blueprint, spec: &StackSpec) -> Result<()> {
    let Some(status) = read_stack_status(bp, &spec.name)? else {
        return Ok(());
    };

    println!("[{}] stack '{}': cleaning up", bp.metadata.name, spec.name);

    let exec_prvkey = exec_prvkey();
    let mut fn_spec = FunctionSpec::new("ensure_stack", "node-agent", "dev");
    fn_spec.conditions.executornames = vec![status.node_name.clone()];
    fn_spec.maxexectime = -1;
    fn_spec.kwargs = HashMap::from([
        (
            "bp_name".into(),
            serde_json::to_value(&bp.metadata.name).unwrap(),
        ),
        (
            "component_name".into(),
            serde_json::to_value(&spec.name).unwrap(),
        ),
        (
            "network".into(),
            serde_json::to_value(&spec.network).unwrap(),
        ),
        (
            "volumes".into(),
            serde_json::to_value(&spec.volumes).unwrap(),
        ),
        (
            "services".into(),
            serde_json::to_value(&[] as &[ServiceSpec]).unwrap(),
        ),
        (
            "existing".into(),
            serde_json::to_value(&status.containers).unwrap(),
        ),
    ]);

    let proc = colonyos::submit(&fn_spec, exec_prvkey).await?;
    colonyos::subscribe_process(&proc, SUCCESS, 120, exec_prvkey).await?;
    println!("[{}] stack '{}': cleaned up", bp.metadata.name, spec.name);
    Ok(())
}

fn parse_bp_name(proc: &Process) -> Result<String> {
    proc.spec
        .kwargs
        .get("blueprintName")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("missing 'blueprintName' kwarg"))
}
