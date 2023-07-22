// Reconciles Nodes with the cloud-provider uninitialized taint + no spec.providerID set.
// Schedules a Job that scrapes HC metadata service. If Job is successful, topology labels are set
// and Node is "adopted" by setting spec.providerID to hcloud://<server id>
// One benefit of this approach is a HC node can

use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Node, NodeAddress, Pod};
use k8s_openapi::serde::Deserialize;
use kube::api::{DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams};
use kube::runtime::conditions;
use kube::runtime::controller::Action;
use kube::runtime::wait::await_condition;
use kube::Client;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::error::Elapsed;
use tracing::info;

pub struct Data {
    pub client: Client,
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
    let deleted = node.metadata.deletion_timestamp.is_some();
    let provider_id = &node
        .spec
        .as_ref()
        .ok_or_else(|| Error::NodeMissingSpec)?
        .provider_id;

    let node_name = node.metadata.name.as_ref().unwrap();
    let node_uid = node.metadata.uid.as_ref().unwrap();

    let job_name = format!("hcc-metadata-{}", node_uid);

    let jobs = kube::Api::<Job>::default_namespaced(client.clone());
    let pods = kube::Api::<Pod>::default_namespaced(client.clone());
    let nodes = kube::Api::<Node>::all(client.clone());

    let job = jobs.get_opt(&job_name).await?;

    let taints = node.spec.as_ref().unwrap().clone().taints.unwrap_or(vec![]);
    let tainted = taints.iter().any(|x| x.key.eq(crate::TAINT_UNINITIALIZED));

    if deleted || provider_id.is_some() || !tainted {
        if let Some(job) = job {
            info!("deleting job {:?}", job.metadata.name);
            jobs.delete(&job_name, &DeleteParams::background()).await?;
        }
        return Ok(Action::await_change());
    }

    if job.is_none() {
        let data = serde_json::from_value(serde_json::json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {
                "name": &job_name,
            },
            "spec": {
                "template": {
                    "spec": {
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
                    },
                },
                "ttlSecondsAfterFinished": 600,
            },
        }))?;
        jobs.create(&PostParams::default(), &data).await?;
    }

    let cond = await_condition(jobs.clone(), &job_name, conditions::is_job_completed());
    let _ = tokio::time::timeout(Duration::from_secs(15), cond).await?;

    let job_pods = pods
        .list(&ListParams::default().labels(&("job-name=".to_string() + &job_name)))
        .await?;
    let mut job_pod = None;
    for pod in job_pods {
        if let Some(status) = pod.status {
            if let Some(phase) = status.phase {
                if phase == "Succeeded" {
                    job_pod = Some(pod.metadata.name.unwrap());
                }
            }
        }
    }

    let job_pod = job_pod.ok_or_else(|| Error::JobMissingPod)?;
    let output = pods.logs(&job_pod, &LogParams::default()).await?;
    let metadata = serde_yaml::Value::deserialize(serde_yaml::Deserializer::from_str(&output))?;

    let node_status = node.status.as_ref().unwrap();

    if let Some(serde_yaml::Value::String(public_ipv4)) = metadata.get("public-ipv4") {
        let mut addresses = node_status.addresses.clone().unwrap_or(vec![]);
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
                    &Patch::Merge(serde_json::json!({
                        "status": {
                            "addresses": addresses,
                        }
                    })),
                )
                .await?;
        }
    }

    if let Some(serde_yaml::Value::String(az)) = metadata.get("availability-zone") {
        if let Some(zone) = az.split('-').next() {
            if let Some(serde_yaml::Value::String(region)) = metadata.get("region") {
                let labels = node.metadata.labels.clone().unwrap_or(BTreeMap::default());
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
                            &Patch::Merge(serde_json::json!({
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

    let id = metadata
        .get("instance-id")
        .ok_or_else(|| Error::MetadataMissing("instance-id".to_string()))?
        .as_u64()
        .unwrap();

    info!(
        "patching Node {} with provider ID and removing taint",
        node_name
    );
    nodes.patch(node_name, &PatchParams::default(), &Patch::Merge(serde_json::json!({
        "spec": {
            "providerID": format!("hcloud://{}", id),
            "taints": taints.iter().filter(|x| x.key != crate::TAINT_UNINITIALIZED).collect::<Vec<_>>(),
        },
    }))).await?;

    //  * set internal IPs if any private-networks
    Ok(Action::await_change())
}

pub fn error_policy(_object: Arc<Node>, _error: &Error, _ctx: Arc<Data>) -> Action {
    Action::requeue(Duration::from_secs(1))
}
