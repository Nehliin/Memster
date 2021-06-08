use anyhow::{anyhow, Result};
use config::File;
use config::{Config, Environment};
use parse_size::parse_size;
use serde::Deserialize;
use serde_aux::field_attributes::deserialize_number_from_string;
use std::convert::TryInto;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize)]
pub struct MemsterConfig {
    // only used to parse the actual size string
    #[serde(rename = "pre_allocated")]
    _pre_allocated: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    pub host: String,
    pub ttl: u64,
    #[serde(skip)]
    pub pre_allocted_bytes: usize,
    pub json_logging: bool,
    pub num_shards: Option<usize>, // This could be extented to contain a max allowed payload
}

impl Default for MemsterConfig {
    fn default() -> Self {
        MemsterConfig {
            port: 8080,
            _pre_allocated: "none".into(),
            host: "0.0.0.0".into(),
            pre_allocted_bytes: 0,
            ttl: 30 * 30 * 24 * 30, // 30 days
            num_shards: None,
            json_logging: true,
        }
    }
}

impl MemsterConfig {
    /// Loads configuration from a config file expected to be present in the root folder
    pub fn load() -> Result<Self> {
        let mut config = Config::default();
        let folder_path = std::env::current_dir().expect("Failed to figure out current directory");
        // Read config:
        config.merge(File::from(folder_path.join("config")).required(true))?;
        // Allows ENV variables to override the yaml settings
        // ex MEMSTER_PORT=1337
        config.merge(Environment::with_prefix("memster").separator("_"))?;
        let mut config: MemsterConfig = config.try_into()?;
        config.pre_allocted_bytes = parse_size(&config._pre_allocated)
            .map_err(|err| anyhow!("Failed to parse pre_allocated field {}", err))?
            .try_into()?;
        Ok(config)
    }
}

pub fn setup_tracing(json_formatting: bool) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    if json_formatting {
        tracing_subscriber::fmt()
            .json()
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_env_filter(filter)
            .init();
    }
}
