use futures::StreamExt;
use hcc::adoption_reconciler;
use hcc::adoption_reconciler::Data;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Node;
use kube::runtime::{watcher, Controller};
use kube::Client;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let client = Client::try_default().await?;

    let nodes = kube::Api::<Node>::all(client.clone());
    let jobs = kube::Api::<Job>::all(client.clone());

    Controller::new(nodes, watcher::Config::default())
        .owns(jobs, watcher::Config::default())
        .shutdown_on_signal()
        .run(
            adoption_reconciler::reconcile,
            adoption_reconciler::error_policy,
            Arc::new(Data { client }),
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {}", o.0.name),
                Err(e) => warn!("reconciliation error: {:?}", e),
            }
        })
        .await;

    Ok(())
}
