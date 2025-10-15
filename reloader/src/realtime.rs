use anyhow::anyhow;
use tracing::{debug, error, info, warn};
use tokio::{
  sync::mpsc,
};
use async_trait::async_trait;

use super::{
  Reload,
  ReloadResult,
  ReloaderError,
  ReloaderService,
  WatchEvent,
  WatchStrategy,
};

/// Handle for realtime watching that receives change events
pub struct RealtimeWatchHandle<V> {
  /// Receiver for watch events
  pub rx: mpsc::Receiver<WatchEvent<V>>,
  /// Cleanup resources when dropped
  _cleanup: Option<Box<dyn std::any::Any + Send>>,
}

impl<V> RealtimeWatchHandle<V> {
  /// Create a new realtime watch handle
  pub fn new(rx: mpsc::Receiver<WatchEvent<V>>) -> Self {
    Self { rx, _cleanup: None }
  }

  /// Create a new realtime watch handle with cleanup resource
  pub fn with_cleanup(rx: mpsc::Receiver<WatchEvent<V>>, cleanup: Box<dyn std::any::Any + Send>) -> Self {
    Self {
      rx,
      _cleanup: Some(cleanup),
    }
  }
}

/// Trait for reloaders that support realtime event-based monitoring.
///
/// This trait extends `Reload` to provide efficient, event-driven updates
/// for data sources that support change notifications (e.g., file system events).
#[async_trait]
pub trait RealtimeWatch<V, S = &'static str>: Reload<V, S>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /// Set up realtime watching for the data source
  ///
  /// Returns a handle that receives change notifications as they occur.
  async fn watch_realtime(&self) -> ReloadResult<RealtimeWatchHandle<V>, V, S>;
}


/// Extension methods that cover realtime and hybrid watching strategies.
impl<T, V, S> ReloaderService<T, V, S>
where
  T: RealtimeWatch<V, S> + Clone,
  V: Eq + PartialEq + Clone,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /// Start monitoring according to the configured strategy when realtime support is available.
  pub async fn start_with_realtime(&self) -> ReloadResult<(), V, S> {
    match self.config.strategy {
      WatchStrategy::Polling => {
        info!("Starting reloader service in polling mode");
        self.start_polling().await
      }
      WatchStrategy::Realtime => {
        info!("Starting reloader service in realtime mode");
        self.start_realtime().await
      }
      WatchStrategy::Hybrid => {
        info!("Starting reloader service in hybrid mode");
        self.start_hybrid().await
      }
    }
  }

  /// Drive the realtime-only monitoring loop.
  async fn start_realtime(&self) -> ReloadResult<(), V, S> {
    debug!("Loading initial value before starting realtime mode");
    if !self.execute_reload_cycle("initial value loading").await? {
      return Ok(());
    }

    let mut handle = self.reloader.watch_realtime().await?;
    debug!("Realtime watching established");

    self.run_realtime_loop(&mut handle).await
  }

  /// Run the hybrid strategy (realtime with polling fallback).
  async fn start_hybrid(&self) -> ReloadResult<(), V, S> {
    debug!("Hybrid mode active (realtime with polling fallback)");

    let mut pending_handle: Option<RealtimeWatchHandle<V>> = None;

    loop {
      if pending_handle.is_none() {
        debug!("Hybrid mode: running reload cycle before realtime attempt");
        if !self.execute_reload_cycle("hybrid pre-realtime reload").await? {
          return Ok(());
        }

        match self.reloader.watch_realtime().await {
          Ok(handle) => {
            info!("Realtime watching established");
            pending_handle = Some(handle);
          }
          Err(e) => {
            warn!("Failed to establish realtime watching: {}. Entering polling fallback.", e);
            pending_handle = match self.run_polling_fallback().await? {
              Some(handle) => Some(handle),
              None => return Ok(()),
            };
            continue;
          }
        }
      }

      let mut handle = pending_handle
        .take()
        .expect("Hybrid mode requires a realtime handle before entering loop");

      if let Err(e) = self.run_realtime_loop(&mut handle).await {
        warn!("Realtime monitoring interrupted: {}. Switching to polling fallback.", e);
        pending_handle = match self.run_polling_fallback().await? {
          Some(handle) => Some(handle),
          None => return Ok(()),
        };
        continue;
      }

      return Ok(());
    }
  }

  /// Continuously handle events coming from the realtime watcher.
  async fn run_realtime_loop(&self, handle: &mut RealtimeWatchHandle<V>) -> ReloadResult<(), V, S> {
    loop {
      match handle.rx.recv().await {
        Some(event) => {
          self.handle_watch_event(event).await?;
        }
        None => {
          warn!("Realtime watch channel closed unexpectedly");
          return Err(ReloaderError::Other(anyhow!("Realtime watch channel closed")));
        }
      }
    }
  }

  /// Polling fallback that keeps watching until realtime monitoring is available again.
  async fn run_polling_fallback(&self) -> ReloadResult<Option<RealtimeWatchHandle<V>>, V, S> {
    info!("Entering polling fallback mode");

    loop {
      if !self.execute_reload_cycle("polling fallback reload").await? {
        info!("Reload cycle requested termination during polling fallback");
        return Ok(None);
      }

      match self.reloader.watch_realtime().await {
        Ok(handle) => {
          info!("Realtime watching restored after polling fallback");
          return Ok(Some(handle));
        }
        Err(e) => {
          warn!("Realtime watching still unavailable: {}", e);
        }
      }

      self.sleep_delay().await;
    }
  }

  /// Dispatch a realtime watch event to the appropriate handler.
  async fn handle_watch_event(&self, event: WatchEvent<V>) -> ReloadResult<(), V, S> {
    match event {
      WatchEvent::Changed(value) => {
        debug!("Received change event");
        if self.should_broadcast_update(&value).await? {
          self.broadcast_update(value).await?;
        } else {
          debug!("Value unchanged, skipping broadcast");
        }
      }
      WatchEvent::Removed => {
        warn!("Watch target was removed");
        self.broadcast_removal().await?;
      }
      WatchEvent::Error(err) => {
        error!("Watch error: {}", err);
        return Err(ReloaderError::Other(anyhow!(err)));
      }
    }
    Ok(())
  }
}
