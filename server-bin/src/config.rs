use crate::error::*;
use async_trait::async_trait;
use clap::{Arg, command};
use hot_reload::{RealtimeWatch, RealtimeWatchHandle, Reload, ReloadResult, ReloaderError, ReloaderService, WatchEvent};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use server_lib::{Server, ServerConfig, ServerConfigBuilder, ServerContextBuilder};
use std::{fs, path::PathBuf, sync::Arc};
use tokio::{runtime::Handle, sync::mpsc};
use tracing::{debug, error, warn};

pub async fn parse_opts(runtime_handle: &Handle) -> Result<(ReloaderService<ConfigReloader, ServerConfig>, Server)> {
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
  let (reloader, rx) = ReloaderService::new(config_path, config).await.unwrap();

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
  async fn new(source: &Self::Source) -> ReloadResult<Self, ServerConfig> {
    Ok(Self {
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> ReloadResult<Option<ServerConfig>, ServerConfig> {
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
    ServerConfigBuilder::default().id(val.id).name(val.name).build().unwrap()
  }
}

#[async_trait]
impl RealtimeWatch<ServerConfig> for ConfigReloader {
  async fn watch_realtime(&self) -> ReloadResult<RealtimeWatchHandle<ServerConfig>, ServerConfig> {
    let (tx, rx) = mpsc::channel(100);
    let config_path = self.config_path.clone();

    let watcher = {
      let tx = tx.clone();
      let config_path_for_callback = config_path.clone();
      // Get Tokio runtime handle to spawn tasks from the notify callback thread
      let handle = Handle::current();

      let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let tx = tx.clone();
        let config_path = config_path_for_callback.clone();
        let handle = handle.clone();

        // Spawn async task on Tokio runtime from the notify callback thread
        handle.spawn(async move {
          match res {
            Ok(event) => {
              debug!("File event: {:?}", event);

              match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => match tokio::fs::read_to_string(&config_path).await {
                  Ok(content) => match toml::from_str::<ConfigToml>(&content) {
                    Ok(config_toml) => {
                      let config: ServerConfig = config_toml.into();
                      if let Err(e) = tx.send(WatchEvent::Changed(config)).await {
                        error!("Failed to send changed event: {}", e);
                      }
                    }
                    Err(e) => {
                      warn!("Failed to parse config file: {}", e);
                      if let Err(e) = tx.send(WatchEvent::Error(e.to_string())).await {
                        error!("Failed to send error event: {}", e);
                      }
                    }
                  },
                  Err(e) => {
                    error!("Failed to read config file: {}", e);
                    if let Err(e) = tx.send(WatchEvent::Error(e.to_string())).await {
                      error!("Failed to send error event: {}", e);
                    }
                  }
                },
                EventKind::Remove(_) => {
                  warn!("Config file was removed");
                  if let Err(e) = tx.send(WatchEvent::Removed).await {
                    error!("Failed to send removed event: {}", e);
                  }
                }
                _ => {
                  debug!("Ignoring event kind: {:?}", event.kind);
                }
              }
            }
            Err(e) => {
              error!("Watch error: {}", e);
              if let Err(e) = tx.send(WatchEvent::Error(e.to_string())).await {
                error!("Failed to send error event: {}", e);
              }
            }
          }
        });
      })
      .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
        .watch(&config_path, RecursiveMode::NonRecursive)
        .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
    };

    debug!("File watching established for: {:?}", config_path);

    Ok(RealtimeWatchHandle::with_cleanup(rx, Box::new(watcher)))
  }
}
