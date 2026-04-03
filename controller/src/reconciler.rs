use std::collections::HashMap;

use anyhow::{Context, Ok, Result, anyhow};
use bollard::plugin::ContainerSummary;
use colonyos::core::{Blueprint, Executor, FunctionSpec, Process, SUCCESS};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::executor::exec_prvkey;

#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerDeploymentSpec {
    pub name: String,
    pub network: Option<String>,
    pub volumes: Vec<String>,
    pub containers: Vec<ContainerSpec>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecutorDeploymentSpec {
    pub name: String,
    pub image: String,
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub mounts: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[allow(dead_code)]
fn default_replicas() -> u32 {
    1
}

pub async fn reconcile(proc: &Process) -> Result<Vec<String>> {
    let (kind, name) = parse_bp_kwargs(proc)?;

    let blueprints: Vec<Blueprint> = colonyos::get_blueprints("dev", &kind, "", exec_prvkey())
        .await?
        .into_iter()
        .filter(|bp| name.as_deref().map_or(true, |n| bp.metadata.name == n))
        .collect();

    for bp in blueprints {
        reconcile_bp(&bp).await?;
    }

    Ok(vec![])
}

async fn reconcile_bp(bp: &Blueprint) -> Result<()> {
    let spec = serde_json::to_value(&bp.spec)?;
    match bp.kind.as_str() {
        "DockerDeployment" => {
            let spec: DockerDeploymentSpec = serde_json::from_value(spec)?;
            reconcile_docker(bp, &spec).await
        }
        "ExecutorDeployment" => {
            let spec: ExecutorDeploymentSpec = serde_json::from_value(spec)?;
            reconcile_executor(bp, &spec).await
        }
        other => Err(anyhow::anyhow!("unsupported blueprint kind: {}", other)),
    }
}

async fn reconcile_docker(bp: &Blueprint, spec: &DockerDeploymentSpec) -> Result<()> {
    println!("DOCKER DEPLOYMENT {spec:#?}");
    Ok(())
}

async fn reconcile_executor(bp: &Blueprint, spec: &ExecutorDeploymentSpec) -> Result<()> {
    println!("EXECUTOR DEPLOYMENT");

    get_all_containers_for_bp(bp).await?;

    Ok(())
}

pub async fn cleanup(proc: &Process) -> Result<Vec<String>> {
    let (kind, name) = parse_bp_kwargs_required(proc)?;

    println!("CLEANUP: {kind} - {name}");
    Ok(vec![])
}

fn parse_bp_kwargs(proc: &Process) -> anyhow::Result<(String, Option<String>)> {
    let kind = proc
        .spec
        .kwargs
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("malformed BP, 'kind' kwarg missing"))
        .map(ToString::to_string)?;

    let name = proc
        .spec
        .kwargs
        .get("blueprintName")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Ok((kind, name))
}

fn parse_bp_kwargs_required(proc: &Process) -> anyhow::Result<(String, String)> {
    let (kind, name) = parse_bp_kwargs(proc)?;
    let name =
        name.ok_or_else(|| anyhow::anyhow!("malformed BP, 'blueprintName' kwarg missing"))?;
    Ok((kind, name))
}

async fn get_all_containers_for_bp(bp: &Blueprint) -> Result<()> {
    println!("getting all containers");
    let exec_prvkey = exec_prvkey();

    let nodes = colonyos::get_executors("dev", exec_prvkey)
        .await?
        .into_iter()
        .filter(|e| e.executortype == "node-agent");

    println!("NODES {:#?}", nodes.clone().collect::<Vec<Executor>>());

    let futures = nodes.map(|node| {
        let mut fn_spec = FunctionSpec::new("list", "node-agent", "dev");
        fn_spec.conditions.executornames = vec![node.executorname];
        fn_spec.kwargs = HashMap::from([(
            "bp_name".into(),
            serde_json::to_value(&bp.metadata.name).unwrap(),
        )]);

        async move {
            let proc = colonyos::submit(&fn_spec, exec_prvkey).await?;
            colonyos::subscribe_process(&proc, SUCCESS, 10, exec_prvkey).await?;
            colonyos::get_process(&proc.processid, exec_prvkey).await
        }
    });

    let containers = try_join_all(futures)
        .await?
        .into_iter()
        .map(|p| {
            let s = p
                .output
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("process had no output"))?;
            serde_json::from_str::<ContainerSummary>(&s)
                .context("failed to parse process output as JSON")
        })
        .collect::<Result<Vec<_>, _>>()?;

    println!("{:#?}", containers);
    Ok(())
}
