use cosmic_config::{CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, CosmicConfigEntry)]
pub struct CosmicIdleConfig {
    /// Screen off idle time, in ms
    pub screen_off_time: Option<u32>,
    /// Suspend idle time when on battery, in ms
    pub suspend_on_battery_time: Option<u32>,
    /// Suspend idle time when on ac, in ms
    pub suspend_on_ac_time: Option<u32>,
}

impl Default for CosmicIdleConfig {
    fn default() -> Self {
        Self {
            screen_off_time: Some(15 * 60 * 1000),
            suspend_on_battery_time: Some(15 * 60 * 1000),
            suspend_on_ac_time: Some(30 * 60 * 1000),
        }
    }
}
