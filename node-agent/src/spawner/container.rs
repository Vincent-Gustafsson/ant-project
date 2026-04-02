use std::collections::HashMap;

use age::{
    Decryptor,
    x25519::{self, Identity},
};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use bollard::{
    Docker,
    plugin::{ContainerCreateBody, ContainerSummary},
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptionsBuilder,
        RemoveContainerOptions, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;
use serde_json::Value;
use std::io::Read;

pub fn decrypt_env(
    kwargs: &HashMap<String, Value>,
    identity: &x25519::Identity,
) -> Result<HashMap<String, String>> {
    let b64 = kwargs
        .get("env")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing or invalid 'env' in kwargs"))?;

    println!("{b64:?}");

    let ciphertext = B64.decode(b64)?;

    let decryptor = Decryptor::new(ciphertext.as_slice())?;
    let mut plaintext = vec![];
    decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))?
        .read_to_end(&mut plaintext)?;

    Ok(rmp_serde::from_slice(&plaintext)?)
}

pub async fn spawn(
    kwargs: HashMap<String, Value>,
    identity: &Identity,
    docker: &Docker,
) -> Result<Vec<String>> {
    let image = kwargs
        .get("image")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("spawn requires an 'image' kwarg"))?
        .to_string();

    let env = decrypt_env(&kwargs, identity)?;
    println!("{env:?}");

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
    println!("{containers:#?}");
    containers
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}
