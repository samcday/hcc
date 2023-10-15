// Reconciles Nodes with the cloud-provider uninitialized taint + no spec.providerID set.
// Schedules a Job that scrapes HC metadata service.
// If Job is successful, metadata information is used to set:
//  * topology labels (if enabled)
//  * ExternalIP in .status.addresses (if enabled)
// The Node is "adopted" by setting spec.providerID to hcloud://<server id>
// Other reconcilers will only deal with Nodes that have a hcloud:// providerID set (and use the ID)

// TODO:
//  * Set InternalIP if a private network ID is configured, and the server is attached to it.

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Node, NodeAddress, Pod, PodSpec};
use k8s_openapi::DeepMerge;
use kube::api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams};
use kube::runtime::conditions;
use kube::runtime::controller::Action;
use kube::runtime::wait::await_condition;
use kube::{Api, Client};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use thiserror::Error;
use tokio::time::error::Elapsed;
use tracing::info;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AdoptionConfig {
    pub enabled: bool,
    pub pod_spec: PodSpec,
    pub topology: bool,
    pub external_address: bool,
}

impl Default for AdoptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pod_spec: PodSpec::default(),
            topology: true,
            external_address: true,
        }
    }
}

pub struct Data {
    pub client: Client,
    pub config: AdoptionConfig,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Node missing .spec")]
    NodeMissingSpec,
    #[error("k8s error: {0}")]
    KubeError(#[source] kube::Error),
    #[error("json error: {0}")]
    SerdeJsonError(#[source] serde_json::Error),
    #[error("yaml error: {0}")]
    SerdeYamlError(#[source] serde_yaml::Error),
    #[error("timout waiting for job to complete: {0}")]
    JobTimeout(#[source] Elapsed),
    #[error("couldn't find completed Pod for Job")]
    JobMissingPod,
    #[error("hcloud metadata missing: {0}")]
    MetadataMissing(String),
}

impl From<kube::Error> for Error {
    fn from(v: kube::Error) -> Self {
        Error::KubeError(v)
    }
}

impl From<serde_json::Error> for Error {
    fn from(v: serde_json::Error) -> Self {
        Error::SerdeJsonError(v)
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(v: serde_yaml::Error) -> Self {
        Error::SerdeYamlError(v)
    }
}

impl From<Elapsed> for Error {
    fn from(v: Elapsed) -> Self {
        Error::JobTimeout(v)
    }
}

pub async fn reconcile(node: Arc<Node>, ctx: Arc<Data>) -> Result<Action, Error> {
    let client = &ctx.client;

    let node_name = node.metadata.name.as_ref().unwrap();
    let node_uid = node.metadata.uid.as_ref().unwrap();

    let job_name = format!("hcc-metadata-{}", node_uid);

    let pods = Api::<Pod>::default_namespaced(client.clone());
    let nodes = Api::<Node>::all(client.clone());

    if let Some(result) = ensure_job(&node, &ctx, client, &job_name).await? {
        return Ok(result);
    }

    let node_status = node.status.as_ref().unwrap();
    let metadata = job_metadata(&job_name, pods).await?;

    if ctx.config.external_address {
        if let Some(Value::String(public_ipv4)) = metadata.get("public-ipv4") {
            let mut addresses = node_status.addresses.clone().unwrap_or_default();
            if !addresses.iter().any(|x| x.type_.eq("ExternalIP")) {
                info!(
                "patching Node {} with external IP {}",
                node_name, public_ipv4
            );
                addresses.push(NodeAddress {
                    type_: "ExternalIP".to_string(),
                    address: public_ipv4.clone(),
                });
                nodes
                    .patch_status(
                        node_name,
                        &PatchParams::default(),
                        &Patch::Merge(json!({
                        "status": {
                            "addresses": addresses,
                        }
                    })),
                    )
                    .await?;
            }
        }
    }

    if ctx.config.topology {
        if let Some(Value::String(az)) = metadata.get("availability-zone") {
            if let Some(zone) = az.split('-').next() {
                if let Some(Value::String(region)) =  metadata.get("region") {
                    let labels = node.metadata.labels.clone().unwrap_or_default();
                    if !labels.contains_key(crate::LABEL_ZONE)
                        && !labels.contains_key(crate::LABEL_REGION)
                    {
                        info!(
                            "patching Node {} with topology {} {}",
                            node_name, region, zone
                        );
                        nodes
                            .patch_metadata(
                                node_name,
                                &PatchParams::default(),
                                &Patch::Merge(json!({
                                    "metadata": {
                                        "labels": {
                                            crate::LABEL_REGION: format!("hcloud-{}", region),
                                            crate::LABEL_ZONE: format!("hcloud-{}", zone),
                                        },
                                    },
                                })),
                            )
                            .await?;
                    }
                }
            }
        }
    }

    let id = metadata
        .get("instance-id")
        .ok_or_else(|| Error::MetadataMissing("instance-id".to_string()))?
        .as_u64()
        .unwrap();

    info!(
        "patching Node {} with provider ID and removing taint",
        node_name
    );

    let taints = node.spec.as_ref().unwrap().clone().taints.unwrap_or_default();
    nodes.patch(node_name, &PatchParams::default(), &Patch::Merge(json!({
        "spec": {
            "providerID": format!("hcloud://{}", id),
            "taints": taints.iter().filter(|x| x.key != crate::TAINT_UNINITIALIZED).collect::<Vec<_>>(),
        },
    }))).await?;

    Ok(Action::await_change())
}

// If the node is tainted and doesn't yet have a providerID, make sure the Job to scrape metadata
// is created and has completed successfully.
// Otherwise, make sure the Job *doesn't* exist (i.e is deleted).
async fn ensure_job(node: &Arc<Node>, ctx: &Arc<Data>, client: &Client, job_name: &String) -> Result<Option<Action>, Error> {
    let jobs = kube::Api::<Job>::default_namespaced(client.clone());
    let node_name = node.metadata.name.as_ref().unwrap();

    let provider_id = &node
        .spec
        .as_ref()
        .ok_or_else(|| Error::NodeMissingSpec)?
        .provider_id;

    let job = jobs.get_opt(job_name).await?;

    let taints = node.spec.as_ref().unwrap().clone().taints.unwrap_or_default();
    let tainted = taints.iter().any(|x| x.key.eq(crate::TAINT_UNINITIALIZED));

    let deleted = node.metadata.deletion_timestamp.is_some();
    if deleted || provider_id.is_some() || !tainted {
        // Node is being deleted, or is no longer tainted, make sure the Job no longer exists.
        if let Some(job) = job {
            info!("deleting metadata job {:?}", job.metadata.name);
            jobs.delete(job_name, &DeleteParams::background()).await?;
        }
        return Ok(Some(Action::await_change()));
    }

    if job.is_none() {
        let mut pod_spec: PodSpec = serde_json::from_value(json!({
            "containers": [{
                "name": "metadata",
                "image": "curlimages/curl:latest",
                "args": [
                    "-s",
                    "--fail",
                    "--retry",
                    "3",
                    "http://169.254.169.254/hetzner/v1/metadata",
                ],
            }],
            "hostNetwork": true,
            "nodeName": node_name,
            "restartPolicy": "Never",
            "tolerations": [{
                "operator": "Exists",
            }],
        }))?;
        pod_spec.merge_from(ctx.config.pod_spec.clone());

        let data = serde_json::from_value(json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": &job_name,
            },
            "spec": {
                "template": {
                    "spec": pod_spec,
                },
                "ttlSecondsAfterFinished": 600,
            },
        }))?;

        info!("scheduling metadata job for node {}", node_name);
        jobs.create(&PostParams::default(), &data).await?;
    }

    let cond = await_condition(jobs.clone(), job_name, conditions::is_job_completed());
    let _ = tokio::time::timeout(Duration::from_secs(15), cond).await?;

    Ok(None)
}

// Scrape the metadata YAML from the successful metadata job pod.
async fn job_metadata(job_name: &String, pods: Api<Pod>) -> Result<Value, Error> {
    let job_pods = pods
        .list(&ListParams::default().labels(&("job-name=".to_string() + &job_name)))
        .await?;
    let mut job_pod = None;
    for pod in job_pods {
        if let Some(phase) = pod.status.and_then(|s| s.phase) {
            if phase == "Succeeded" {
                job_pod = Some(pod.metadata.name.unwrap());
            }
        }
    }

    let job_pod = job_pod.ok_or_else(|| Error::JobMissingPod)?;
    let output = pods.logs(&job_pod, &LogParams::default()).await?;
    let metadata = Value::deserialize(serde_yaml::Deserializer::from_str(&output))?;
    Ok(metadata)
}

pub fn error_policy(_object: Arc<Node>, _error: &Error, _ctx: Arc<Data>) -> Action {
    Action::requeue(Duration::from_secs(1))
}
