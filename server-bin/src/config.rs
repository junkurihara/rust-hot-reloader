use crate::{error::*, log::*};
use async_trait::async_trait;
use clap::{command, Arg};
use reload::{Reload, ReloaderError, ReloaderService};
use serde::Deserialize;
use server_lib::{Server, ServerConfig, ServerConfigBuilder, ServerContextBuilder};
use std::{
  fs,
  path::PathBuf,
  sync::{Arc, Mutex},
};
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

  /////////////////////////
  //   toml file path    //
  /////////////////////////
  let config_path = matches.get_one::<String>("config_file").unwrap();

  // Setup reloader service
  let (reloader, rx) = ReloaderService::new(config_path, 10).await.unwrap();

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
  pub current_config: Arc<Mutex<Option<ConfigToml>>>,
  pub config_path: PathBuf,
}

#[async_trait]
impl Reload<ServerConfig> for ConfigReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError> {
    Ok(Self {
      current_config: Arc::new(Mutex::new(None)),
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> Result<Option<Arc<ServerConfig>>, ReloaderError> {
    let config_str = fs::read_to_string(&self.config_path).context("Failed to read config file")?;
    let config_toml: ConfigToml = toml::from_str(&config_str)
      .context("Failed to parse toml config")
      .map_err(|_e| ReloaderError::Reload("Failed to load the configuration file"))?;

    {
      let mut current_opt = self
        .current_config
        .lock()
        .map_err(|_e| ReloaderError::Reload("Failed to lock the mutex for loading current config"))?;
      if let Some(current) = current_opt.clone() {
        if !current.is_updated(&config_toml) {
          return Ok(None);
        }
      }

      *current_opt = Some(config_toml.clone());
    }
    info!("Configuration file was initially loaded or updated");

    let config = config_toml.into();

    Ok(Some(Arc::new(config)))
  }
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, Clone)]
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

impl ConfigToml {
  pub fn is_updated(&self, new_one: &Self) -> bool {
    self != new_one
  }
}
