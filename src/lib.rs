pub mod adoption_reconciler;

pub const TAINT_UNINITIALIZED: &str = "node.cloudprovider.kubernetes.io/uninitialized";
pub const LABEL_ZONE: &str = "topology.kubernetes.io/zone";
pub const LABEL_REGION: &str = "topology.kubernetes.io/region";
