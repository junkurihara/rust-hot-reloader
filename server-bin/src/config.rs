use crate::error::*;
use async_trait::async_trait;
use clap::{Arg, command};
use hot_reload::{AsyncFileLoad, ReloaderService};
use serde::Deserialize;
use server_lib::{Server, ServerConfig, ServerConfigBuilder, ServerContextBuilder};
use std::{
  fs,
  path::{Path, PathBuf},
  sync::Arc,
};
use tokio::runtime::Handle;

/// Parse command line options and setup configuration reloader and server context.
pub async fn parse_opts(
  runtime_handle: &Handle,
) -> Result<(
  ReloaderService<ConfigReloader, ConfigToml, String>,
  Server<ConfigToml, String>,
)> {
  let _ = include_str!("../Cargo.toml");
  let options = command!()
    .arg(
      Arg::new("config_file")
        .long("config")
        .short('c')
        .value_name("FILE")
        .required(true)
        .help("Configuration file path like 'config.toml'"),
    )
    .arg(
      Arg::new("watch_mode")
        .long("watch-mode")
        .short('w')
        .value_name("MODE")
        .default_value("hybrid")
        .help("Watch strategy: polling, realtime, or hybrid (default)"),
    );
  let matches = options.get_matches();

  // toml file path
  let config_path = matches.get_one::<String>("config_file").unwrap();

  // Parse watch mode
  let watch_mode = matches.get_one::<String>("watch_mode").unwrap();
  let config = match watch_mode.as_str() {
    "polling" => {
      tracing::info!("Using polling mode with 10 second interval");
      hot_reload::ReloaderConfig::polling(10)
    }
    "hybrid" => {
      tracing::info!("Using hybrid mode (realtime with polling fallback)");
      hot_reload::ReloaderConfig::hybrid(10)
    }
    "realtime" => {
      tracing::info!("Using realtime file system monitoring");
      hot_reload::ReloaderConfig::realtime()
    }
    _ => {
      tracing::warn!("Unknown watch mode '{}', defaulting to hybrid", watch_mode);
      hot_reload::ReloaderConfig::hybrid(10)
    }
  };

  // Setup reloader service
  let (reloader, rx) = ReloaderService::<ConfigReloader, ConfigToml, String>::new(config_path, config)
    .await
    .unwrap();

  // Setup server context with arbitrary config reloader's receiver
  let context = ServerContextBuilder::default()
    .runtime_handle(runtime_handle.to_owned())
    .context_rx(rx)
    .build()?;

  let server = Server::<ConfigToml, String> {
    context: Arc::new(context),
  };

  Ok((reloader, server))
}

pub type ConfigReloader = hot_reload::file_reloader::FileReloader<ConfigToml>;

#[derive(Debug, Default, Deserialize, Clone, PartialEq, Eq)]
/// Intermediate struct to deserialize the TOML configuration.
pub struct ConfigToml {
  pub id: u32,
  pub name: String,
}

impl TryFrom<&PathBuf> for ConfigToml {
  type Error = String;

  fn try_from(path: &PathBuf) -> Result<Self, Self::Error> {
    let config_str = fs::read_to_string(path).map_err(|e| format!("Failed to read config file: {}", e))?;
    let config_toml: ConfigToml = toml::from_str(&config_str).map_err(|e| format!("Failed to parse toml config: {}", e))?;
    Ok(config_toml)
  }
}

#[async_trait]
impl AsyncFileLoad for ConfigToml {
  type Error = String;

  async fn async_load_from<T>(path: T) -> Result<Self, Self::Error>
  where
    T: AsRef<Path> + Send,
  {
    let config_str = tokio::fs::read_to_string(path)
      .await
      .map_err(|e| format!("Failed to read config file: {}", e))?;
    let config_toml: ConfigToml = toml::from_str(&config_str).map_err(|e| format!("Failed to parse toml config: {}", e))?;
    Ok(config_toml)
  }
}

impl From<ConfigToml> for ServerConfig {
  fn from(val: ConfigToml) -> Self {
    ServerConfigBuilder::default().id(val.id).name(val.name).build().unwrap()
  }
}
