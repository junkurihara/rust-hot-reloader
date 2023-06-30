use crate::{error::*, log::*};
use async_trait::async_trait;
use clap::{command, Arg};
use reload::{ReloaderError, ReloaderService, ReloaderTarget};
use serde::Deserialize;
use server_lib::{Server, ServerContextBuilder};
use std::{fs, sync::Arc};
use tokio::runtime::Handle;

pub async fn parse_opts(runtime_handle: &Handle) -> Result<(ReloaderService<ServerConfig>, Server<ServerConfig>)> {
  // Setup watcher service
  let watcher_target = ServerConfig {
    config: ConfigToml {
      listen_addresses: None,
      user_info: None,
    },
  };
  let (reloader, rx) = ReloaderService::new(watcher_target, 10).await.unwrap();

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

#[derive(Debug, Default, Deserialize, PartialEq, Eq, Clone)]
pub struct ConfigToml {
  pub listen_addresses: Option<Vec<String>>,
  pub user_info: Option<UserInfo>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
  pub config: ConfigToml,
}

#[async_trait]
impl ReloaderTarget for ServerConfig {
  type TargetValue = ConfigToml;
  async fn reload(&self) -> Result<Option<Arc<Self::TargetValue>>, ReloaderError> {
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
