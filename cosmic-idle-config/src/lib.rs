use cosmic_config::{cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub enum IdleAction {
    ScreenOff,
    Command(Vec<String>),
}

#[derive(Debug, Deserialize, Serialize, Clone, CosmicConfigEntry)]
pub struct CosmicIdleConfig {
    pub time: u32,
    pub action: IdleAction,
}

impl Default for CosmicIdleConfig {
    fn default() -> Self {
        Self {
            time: 60 * 10,
            action: IdleAction::ScreenOff,
        }
    }
}
