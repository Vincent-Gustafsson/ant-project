use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct AntConfig {
    pub colony_server_url: String,
    pub colony_name: String,
    pub private_key: String,
}

impl Default for AntConfig {
    fn default() -> Self {
        Self {
            colony_server_url: "".to_string(),
            colony_name: "".to_string(),
            private_key: "".to_string(),
        }
    }
}

impl AntConfig {
    pub fn load() -> Result<Self> {
        let cfg: AntConfig = confy::load("ant", "config")?;
        confy::store("ant", "config", &cfg)?;
        Ok(cfg)
    }
}
