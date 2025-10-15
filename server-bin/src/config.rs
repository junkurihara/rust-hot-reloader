use crate::error::*;
use async_trait::async_trait;
use clap::{Arg, command};
use hot_reload::{RealtimeWatch, RealtimeWatchHandle, Reload, ReloadResult, ReloaderError, ReloaderService, WatchEvent};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use server_lib::{Server, ServerConfig, ServerConfigBuilder, ServerContextBuilder};
use std::{
  fs,
  path::PathBuf,
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};
use tokio::{
  runtime::Handle,
  sync::{Mutex, mpsc},
};
use tracing::{debug, error, warn};

/// Default channel size for mpsc channels
const DEFAULT_CHANNEL_SIZE: usize = 100;

/// Parse command line options and setup configuration reloader and server context.
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
/// Intermediate struct to deserialize the TOML configuration.
pub struct ConfigToml {
  pub id: u32,
  pub name: String,
}

impl From<ConfigToml> for ServerConfig {
  fn from(val: ConfigToml) -> Self {
    ServerConfigBuilder::default().id(val.id).name(val.name).build().unwrap()
  }
}

/// Duration to debounce rapid successive file events.
const FILE_EVENT_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Debug)]
/// Simple enum to represent debounced file events.
enum DebouncedEvent {
  Reload,
  Removed,
  Error(String),
}

/// Queue and debounce file events to avoid rapid successive reloads.
async fn queue_debounced_event(
  event: DebouncedEvent,
  debounce_counter: Arc<AtomicU64>,
  latest_event: Arc<Mutex<Option<(u64, DebouncedEvent)>>>,
  tx: mpsc::Sender<WatchEvent<ServerConfig>>,
  config_path: PathBuf,
) {
  let event_id = debounce_counter.fetch_add(1, Ordering::AcqRel) + 1;

  {
    let mut slot = latest_event.lock().await;
    *slot = Some((event_id, event));
  }

  tokio::time::sleep(FILE_EVENT_DEBOUNCE).await;

  if debounce_counter.load(Ordering::Acquire) != event_id {
    return;
  }

  let should_process = {
    let slot = latest_event.lock().await;
    matches!(slot.as_ref(), Some((stored_id, _)) if *stored_id == event_id)
  };

  if !should_process {
    return;
  }

  let event_to_process = {
    let mut slot = latest_event.lock().await;
    slot.take().map(|(_, event)| event)
  };

  if let Some(event) = event_to_process {
    handle_debounced_event(event, &tx, &config_path).await;
  }
}

/// Handle a debounced file event by reading and parsing the config file, then sending appropriate events.
async fn handle_debounced_event(event: DebouncedEvent, tx: &mpsc::Sender<WatchEvent<ServerConfig>>, config_path: &PathBuf) {
  match event {
    DebouncedEvent::Reload => match tokio::fs::read_to_string(config_path).await {
      Ok(content) => match toml::from_str::<ConfigToml>(&content) {
        Ok(config_toml) => {
          let config: ServerConfig = config_toml.into();
          if let Err(e) = tx.send(WatchEvent::Changed(config)).await {
            error!("Failed to send changed event: {}", e);
          }
        }
        Err(e) => {
          warn!("Failed to parse config file: {}", e);
          let message = e.to_string();
          if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
            error!("Failed to send error event: {}", send_err);
          }
        }
      },
      Err(e) => {
        error!("Failed to read config file: {}", e);
        let message = e.to_string();
        if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
          error!("Failed to send error event: {}", send_err);
        }
      }
    },
    DebouncedEvent::Removed => {
      warn!("Config file was removed");
      if let Err(e) = tx.send(WatchEvent::Removed).await {
        error!("Failed to send removed event: {}", e);
      }
    }
    DebouncedEvent::Error(message) => {
      if let Err(e) = tx.send(WatchEvent::Error(message)).await {
        error!("Failed to send error event: {}", e);
      }
    }
  }
}

#[async_trait]
impl RealtimeWatch<ServerConfig> for ConfigReloader {
  /// Establish a file watcher on the configuration file path.
  async fn watch_realtime(&self) -> ReloadResult<RealtimeWatchHandle<ServerConfig>, ServerConfig> {
    let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_SIZE);
    let config_path = self.config_path.clone();
    let debounce_counter = Arc::new(AtomicU64::new(0));
    let latest_event = Arc::new(Mutex::new(None::<(u64, DebouncedEvent)>));

    let watcher = {
      let tx = tx.clone();
      let config_path_for_callback = config_path.clone();
      // Get Tokio runtime handle to spawn tasks from the notify callback thread
      let handle = Handle::current();
      let debounce_counter = debounce_counter.clone();
      let latest_event = latest_event.clone();

      let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let tx = tx.clone();
        let config_path = config_path_for_callback.clone();
        let handle = handle.clone();
        let debounce_counter = debounce_counter.clone();
        let latest_event = latest_event.clone();

        // Spawn async task on Tokio runtime from the notify callback thread
        handle.spawn(async move {
          let event = match res {
            Ok(event) => {
              debug!("File event: {:?}", event);
              match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => Some(DebouncedEvent::Reload),
                EventKind::Remove(_) => Some(DebouncedEvent::Removed),
                _ => {
                  debug!("Ignoring event kind: {:?}", event.kind);
                  None
                }
              }
            }
            Err(e) => {
              error!("Watch error: {}", e);
              Some(DebouncedEvent::Error(e.to_string()))
            }
          };

          if let Some(event) = event {
            queue_debounced_event(event, debounce_counter, latest_event, tx, config_path).await;
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
