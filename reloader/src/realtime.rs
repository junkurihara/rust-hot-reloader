use super::{Reload, ReloadResult, ReloaderError, ReloaderService, WatchEvent, WatchStrategy};
use anyhow::anyhow;
use async_trait::async_trait;
use std::cmp;
use tokio::{
  sync::mpsc,
  time::{Duration, sleep},
};
use tracing::{debug, error, info, warn};

/* ---------------------------------------------------------- */
// Constants

/// Maximum backoff multiplier for hybrid mode polling fallback.
/// The delay doubles after each failed retry attempt until it reaches (base_delay * 8).
const HYBRID_MAX_BACKOFF_MULTIPLIER: u32 = 8;

/* ---------------------------------------------------------- */
// Realtime Watch Handle

/// Handle for realtime watching that receives change events
pub struct RealtimeWatchHandle<V> {
  /// Receiver for watch events
  pub rx: mpsc::Receiver<WatchEvent<V>>,
  /// Optional cleanup resource that will be dropped when this handle is dropped.
  /// This is typically used to store a file watcher or other resource that needs
  /// to be kept alive for the duration of the watch operation.
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

/* ---------------------------------------------------------- */
// RealtimeWatch Trait

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

/* ---------------------------------------------------------- */
// ReloaderService Extensions for Realtime and Hybrid Strategies

/// Extension methods that cover realtime and hybrid watching strategies.
impl<T, V, S> ReloaderService<T, V, S>
where
  T: RealtimeWatch<V, S> + Clone,
  V: Eq + PartialEq + Clone,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /* -------------------- */
  // Public API: Service Entry Point

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

  /* -------------------- */
  // Internal: Strategy Implementations

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
  ///
  /// State machine flow:
  /// 1. Start: No realtime handle exists
  /// 2. Attempt to establish realtime watching
  ///    - 3a. Success: Enter realtime mode, receive events
  ///    - 3b. Failure: Enter polling fallback with exponential backoff
  /// 4. If realtime mode fails: Return to step 2
  /// 5. Polling fallback periodically retries realtime: Jump to step 2
  async fn start_hybrid(&self) -> ReloadResult<(), V, S> {
    debug!("Hybrid mode active (realtime with polling fallback)");

    let mut pending_handle: Option<RealtimeWatchHandle<V>> = None;

    loop {
      // State: No realtime handle - need to establish watching
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
            // Enter polling fallback mode, which will periodically retry realtime
            pending_handle = match self.run_polling_fallback().await? {
              Some(handle) => Some(handle),
              None => return Ok(()),
            };
            continue;
          }
        }
      }

      // State: Have realtime handle - enter event loop
      let mut handle = pending_handle
        .take()
        .expect("Hybrid mode requires a realtime handle before entering loop");

      if let Err(e) = self.run_realtime_loop(&mut handle).await {
        warn!("Realtime monitoring interrupted: {}. Switching to polling fallback.", e);
        // Realtime failed - enter polling fallback and retry
        pending_handle = match self.run_polling_fallback().await? {
          Some(handle) => Some(handle),
          None => return Ok(()),
        };
        continue;
      }

      return Ok(());
    }
  }

  /* -------------------- */
  // Internal: Event Loop Handlers

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

  /* -------------------- */
  // Internal: Polling Fallback with Exponential Backoff

  /// Polling fallback that keeps watching until realtime monitoring is available again.
  ///
  /// Uses exponential backoff to avoid busy looping:
  /// - Starts with the configured watch_delay_sec
  /// - Doubles the delay after each failed retry
  /// - Caps at (watch_delay_sec * HYBRID_MAX_BACKOFF_MULTIPLIER)
  /// - Periodically retries establishing realtime watching
  ///
  /// Returns `Some(handle)` when realtime watching is restored, `None` if service should terminate.
  async fn run_polling_fallback(&self) -> ReloadResult<Option<RealtimeWatchHandle<V>>, V, S> {
    info!("Entering polling fallback mode");

    // Initialize exponential backoff parameters
    let base_delay_secs = self.config.watch_delay_sec.max(1);
    let base_delay = Duration::from_secs(base_delay_secs.into());
    let max_backoff = base_delay.saturating_mul(HYBRID_MAX_BACKOFF_MULTIPLIER);
    let mut current_delay = base_delay;

    loop {
      // Execute polling reload cycle
      if !self.execute_reload_cycle("polling fallback reload").await? {
        info!("Reload cycle requested termination during polling fallback");
        return Ok(None);
      }

      // Attempt to restore realtime watching
      match self.reloader.watch_realtime().await {
        Ok(handle) => {
          info!("Realtime watching restored after polling fallback");
          return Ok(Some(handle));
        }
        Err(e) => {
          warn!("Realtime watching still unavailable: {}. Retrying in {:?}", e, current_delay);
        }
      }

      // Sleep with current delay
      sleep(current_delay).await;

      // Apply exponential backoff: delay = min(delay * 2, max_backoff)
      current_delay = cmp::min(current_delay.saturating_mul(2), max_backoff);
    }
  }

  /* -------------------- */
  // Internal: Event Dispatching

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
        // Be careful with Removed events; they may be transient.
        // If you do not want to treat removals as terminal, consider to emit an WatchEvent::Error instead to trigger fallback.
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
