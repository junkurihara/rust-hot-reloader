use crate::{
  error::*,
  globals::Globals,
  log::*,
  watcher::{WatcherReceiver, WatcherTarget},
};
use async_trait::async_trait;
use clap::{command, Arg};
use serde::Deserialize;
use std::{fs, sync::Arc};
use tokio::runtime::Handle;

pub async fn parse_opts<T>(runtime_handle: &Handle, rx: WatcherReceiver<T>) -> Result<Arc<Globals<T>>>
where
  T: WatcherTarget + Clone,
{
  let _ = include_str!("../Cargo.toml");
  let options = command!().arg(
    Arg::new("config_file")
      .long("config")
      .short('c')
      .value_name("FILE")
      .help("Configuration file path like 'config.toml'"),
  );
  let matches = options.get_matches();

  /////////////////////////
  //   toml file path    //
  /////////////////////////
  let Some(config_path) = matches.get_one::<String>("config_file") else {
    panic!()
  };

  Ok(Arc::new(Globals {
    config_path: config_path.to_string(),
    runtime_handle: runtime_handle.to_owned(),
    watcher_rx: rx,
  }))
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, Clone)]
pub struct ConfigToml {
  pub listen_addresses: Option<Vec<String>>,
  pub user_info: Option<UserInfo>,
}

#[derive(Clone, Debug)]
pub struct ConfigWatch {
  pub config: ConfigToml,
}

#[async_trait]
impl WatcherTarget for ConfigWatch {
  type TargetValue = ConfigToml;
  async fn reload(&self) -> Result<Option<Arc<Self::TargetValue>>> {
    // TODO:
    info!("reload");
    // Ok(None)
    Ok(Some(Arc::new(ConfigToml {
      listen_addresses: None,
      user_info: None,
    })))
  }
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, Clone)]
pub struct UserInfo {
  pub user_name: Option<String>,
  pub user_id: Option<String>,
}

impl ConfigToml {
  pub fn reload(config_file: &str) -> Result<Self> {
    let config_str = if let Ok(s) = fs::read_to_string(config_file) {
      s
    } else {
      bail!("Failed to load config file");
    };
    let parsed: Result<ConfigToml> =
      toml::from_str(&config_str).map_err(|e: toml::de::Error| anyhow!("Failed to parse toml config: {:?}", e));
    parsed
  }

  pub fn is_updated(&self, new_one: &Self) -> bool {
    self == new_one
  }
}
