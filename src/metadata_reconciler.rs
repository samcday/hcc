use crate::MetadataConfig;
use k8s_openapi::api::core::v1::Node;
use kube::runtime::controller::Action;
use kube::Client;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::info;

pub struct Data {
    pub client: Client,
    pub config: MetadataConfig,
    pub token: String,
}

#[derive(Debug, Error)]
pub enum Error {}

pub async fn reconcile(_node: Arc<Node>, _ctx: Arc<Data>) -> Result<Action, Error> {
    info!("reconciling {:?}", _node.metadata.name);
    Ok(Action::await_change())
}

pub fn error_policy(_object: Arc<Node>, _error: &Error, _ctx: Arc<Data>) -> Action {
    Action::requeue(Duration::from_secs(1))
}
