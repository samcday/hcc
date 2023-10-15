use k8s_openapi::serde::{Deserialize, Serialize};
use crate::adoption_reconciler::AdoptionConfig;

pub mod adoption_reconciler;
pub mod metadata_reconciler;

pub const LABEL_ZONE: &str = "topology.kubernetes.io/zone";
pub const LABEL_REGION: &str = "topology.kubernetes.io/region";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MetadataConfig {
    pub enabled: bool,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            enabled: true,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Config {
    pub adoption: AdoptionConfig,
    pub metadata: MetadataConfig,
    pub token: Option<String>,
}
