use crate::error::*;
use async_trait::async_trait;
use clap::{command, Arg};
use hot_reload::{Reload, ReloaderError, ReloaderService};
use serde::Deserialize;
use server_lib::{Server, ServerConfig, ServerConfigBuilder, ServerContextBuilder};
use std::{fs, path::PathBuf, sync::Arc};
use tokio::runtime::Handle;

pub async fn parse_opts(runtime_handle: &Handle) -> Result<(ReloaderService<ConfigReloader, ServerConfig>, Server)> {
  let _ = include_str!("../Cargo.toml");
  let options = command!().arg(
    Arg::new("config_file")
      .long("config")
      .short('c')
      .value_name("FILE")
      .required(true)
      .help("Configuration file path like 'config.toml'"),
  );
  let matches = options.get_matches();

  // toml file path
  let config_path = matches.get_one::<String>("config_file").unwrap();

  // Setup reloader service
  let (reloader, rx) = ReloaderService::new(config_path, 10, false).await.unwrap();

  // Setup server context with arbitrary config reloader's receiver
  let context = ServerContextBuilder::default()
    .runtime_handle(runtime_handle.to_owned())
    .context_rx(rx)
    .build()?;

  Ok((
    reloader,
    Server {
      context: Arc::new(context),
    },
  ))
}

#[derive(Clone, Debug)]
pub struct ConfigReloader {
  pub config_path: PathBuf,
}

#[async_trait]
impl Reload<ServerConfig> for ConfigReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ServerConfig>> {
    Ok(Self {
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> Result<Option<ServerConfig>, ReloaderError<ServerConfig>> {
    let config_str = fs::read_to_string(&self.config_path).context("Failed to read config file")?;
    let config_toml: ConfigToml = toml::from_str(&config_str)
      .context("Failed to parse toml config")
      .map_err(|_e| ReloaderError::Reload("Failed to load the configuration file"))?;
    let config = config_toml.into();

    Ok(Some(config))
  }
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ConfigToml {
  pub id: u32,
  pub name: String,
}

impl From<ConfigToml> for ServerConfig {
  fn from(val: ConfigToml) -> Self {
    ServerConfigBuilder::default()
      .id(val.id)
      .name(val.name)
      .build()
      .unwrap()
  }
}
