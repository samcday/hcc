use k8s_openapi::api::core::v1::PodSpec;
use k8s_openapi::serde::{Deserialize, Serialize};

pub mod adoption_reconciler;
pub mod metadata_reconciler;

pub const TAINT_UNINITIALIZED: &str = "node.cloudprovider.kubernetes.io/uninitialized";
pub const LABEL_ZONE: &str = "topology.kubernetes.io/zone";
pub const LABEL_REGION: &str = "topology.kubernetes.io/region";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AdoptionConfig {
    pub enabled: bool,
    pub pod_spec: PodSpec,
}

impl Default for AdoptionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pod_spec: PodSpec::default(),
        }
    }
}

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
