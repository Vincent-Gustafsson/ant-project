use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Deployment {
    pub pkg_name: String,
    pub pkg_version: String,
    pub revision: i32,
    pub deployed_at: String,
}
