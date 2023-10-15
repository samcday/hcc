use futures::{FutureExt, StreamExt};
use hcc::{adoption_reconciler, metadata_reconciler, Config};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Node;
use kube::runtime::{watcher, Controller};
use kube::Client;
use std::fs::File;
use std::sync::Arc;
use futures::future::join_all;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let client = Client::try_default().await?;

    let nodes = kube::Api::<Node>::all(client.clone());
    let jobs = kube::Api::<Job>::all(client.clone());

    let config = if let Ok(config_filename) = std::env::var("CONFIG") {
        serde_yaml::from_reader(File::open(config_filename)?)?
    } else {
        Config::default()
    };

    let mut futures = vec![];

    if config.adoption.enabled {
        futures.push(
            async {
                Controller::new(nodes.clone(), watcher::Config::default())
                    .owns(jobs.clone(), watcher::Config::default())
                    .shutdown_on_signal()
                    .run(
                        adoption_reconciler::reconcile,
                        adoption_reconciler::error_policy,
                        Arc::new(adoption_reconciler::Data {
                            client: client.clone(),
                            config: config.adoption,
                        }),
                    )
                    .for_each(|res| async move {
                        match res {
                            Ok(o) => info!("reconciled {}", o.0.name),
                            Err(e) => warn!("reconciliation error: {:?}", e),
                        }
                    })
                    .await;
            }
            .boxed(),
        );
    }

    if config.metadata.enabled {
        if config.token.is_none() {
            panic!("metadata reconciler enabled, but no hcloud token provided");
        }

        futures.push(
            async {
                Controller::new(nodes.clone(), watcher::Config::default())
                    .owns(jobs.clone(), watcher::Config::default())
                    .shutdown_on_signal()
                    .run(
                        metadata_reconciler::reconcile,
                        metadata_reconciler::error_policy,
                        Arc::new(metadata_reconciler::Data {
                            client: client.clone(),
                            config: config.metadata,
                            token: config.token.unwrap(),
                        }),
                    )
                    .for_each(|res| async move {
                        match res {
                            Ok(o) => info!("reconciled {}", o.0.name),
                            Err(e) => warn!("reconciliation error: {:?}", e),
                        }
                    })
                    .await;
            }
            .boxed(),
        );
    }

    join_all(futures).await;

    Ok(())
}
