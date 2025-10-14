use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::{
  sync::{Mutex, mpsc, watch},
  time::{Duration, sleep},
};
use tracing::{debug, error, info, warn};

/// Errors that can occur during reloading operations
#[derive(Debug, Error)]
pub enum ReloaderError<V, S = &'static str>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  #[error("Error at reloaded value receiver")]
  WatchRecvError(#[from] watch::error::RecvError),

  #[error("Error at reloaded value sender")]
  WatchSendError(#[from] watch::error::SendError<Option<V>>),

  #[error("Failed to reload: {0}")]
  Reload(S),

  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

/// Type alias for a commonly used Result type
pub type ReloadResult<T, V, S = &'static str> = Result<T, ReloaderError<V, S>>;

/// Event emitted by realtime watchers
#[derive(Debug, Clone)]
pub enum WatchEvent<V> {
  /// Value changed with new content
  Changed(V),
  /// Source was removed or became unavailable
  Removed,
  /// Error occurred during watching
  Error(String),
}

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

/// Trait defining the responsibility of reloaders to periodically load target values from a source.
///
/// The source can be a file, KVS, or any other data source that implements this trait.
/// The generic parameters allow for flexible error handling and value types.
#[async_trait]
pub trait Reload<V, S = &'static str>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  type Source;

  /// Create a new reloader instance from the given source
  async fn new(src: &Self::Source) -> ReloadResult<Self, V, S>
  where
    Self: Sized;

  /// Reload the target value from the source
  async fn reload(&self) -> ReloadResult<Option<V>, V, S>;
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

/// Sender wrapper for broadcasting reloaded values to receivers
pub struct ReloaderSender<V>
where
  V: Eq + PartialEq,
{
  inner: watch::Sender<Option<V>>,
}

impl<V> ReloaderSender<V>
where
  V: Eq + PartialEq,
{
  /// Send a new value to all receivers
  pub fn send(&self, value: Option<V>) -> Result<(), watch::error::SendError<Option<V>>> {
    self.inner.send(value)
  }
}

/// Receiver wrapper for listening to reloaded values
#[derive(Clone)]
pub struct ReloaderReceiver<V, S = &'static str>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  inner: watch::Receiver<Option<V>>,
  _phantom: std::marker::PhantomData<S>,
}

impl<V, S> ReloaderReceiver<V, S>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /// Wait for the next change notification
  pub async fn changed(&mut self) -> ReloadResult<(), V, S> {
    self.inner.changed().await.map_err(ReloaderError::WatchRecvError)
  }

  /// Borrow the current value
  pub fn borrow(&self) -> watch::Ref<'_, Option<V>> {
    self.inner.borrow()
  }

  /// Get a clone of the current value if it exists
  pub fn get(&self) -> Option<V>
  where
    V: Clone,
  {
    self.inner.borrow().clone()
  }
}

/// Strategy for watching and reloading target values
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStrategy {
  /// Use polling-based monitoring (default, works for all data sources)
  Polling,
  /// Use realtime event-based monitoring (requires `RealtimeWatch` implementation)
  Realtime,
  /// Try realtime monitoring first, fall back to polling if it fails and periodically retry realtime
  Hybrid,
}

impl Default for WatchStrategy {
  fn default() -> Self {
    Self::Polling
  }
}

/// Configuration for the reloader service
#[derive(Debug, Clone)]
pub struct ReloaderConfig {
  /// Period between reload attempts in seconds (used in Polling mode)
  pub watch_delay_sec: u32,
  /// If true, broadcast updates even when values haven't changed
  pub force_reload: bool,
  /// Strategy for watching the target value
  pub strategy: WatchStrategy,
}

impl Default for ReloaderConfig {
  fn default() -> Self {
    Self {
      watch_delay_sec: 10,
      force_reload: false,
      strategy: WatchStrategy::Polling,
    }
  }
}

impl ReloaderConfig {
  /// Create a config with polling strategy
  pub fn polling(watch_delay_sec: u32) -> Self {
    Self {
      watch_delay_sec,
      force_reload: false,
      strategy: WatchStrategy::Polling,
    }
  }

  /// Create a config with realtime strategy
  pub fn realtime() -> Self {
    Self {
      watch_delay_sec: 10, // Used as fallback in case of errors
      force_reload: false,
      strategy: WatchStrategy::Realtime,
    }
  }

  /// Create a config with hybrid strategy (realtime + polling fallback)
  pub fn hybrid(watch_delay_sec: u32) -> Self {
    Self {
      watch_delay_sec,
      force_reload: false,
      strategy: WatchStrategy::Hybrid,
    }
  }
}

/// Main service for watching and reloading target values from a source.
///
/// # Example
/// ```ignore
/// let config = ReloaderConfig::default();
/// let (service, rx) = ReloaderService::new(source, config).await?;
///
/// tokio::spawn(async move { service.start().await });
///
/// loop {
///   tokio::select! {
///     _ = some_other_work() => {
///       // Handle other work
///     }
///     _ = rx.changed() => {
///       if let Some(value) = rx.get() {
///         info!("Received updated value: {:?}", value);
///       } else {
///         break; // Service terminated
///       }
///     }
///     else => break,
///   }
/// }
/// ```
pub struct ReloaderService<T, V, S = &'static str>
where
  T: Reload<V, S>,
  V: Eq + PartialEq + Clone,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  reloader: T,
  current_value: Arc<Mutex<Option<V>>>,
  tx: ReloaderSender<V>,
  config: ReloaderConfig,
  _phantom: std::marker::PhantomData<S>,
}

impl<T, V, S> ReloaderService<T, V, S>
where
  T: Reload<V, S> + Clone,
  V: Eq + PartialEq + Clone,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /// Create a new reloader service with the given configuration
  pub async fn new(
    source: &<T as Reload<V, S>>::Source,
    config: ReloaderConfig,
  ) -> ReloadResult<(Self, ReloaderReceiver<V, S>), V, S> {
    let reloader = <T as Reload<V, S>>::new(source).await?;
    let (tx, rx) = watch::channel(None);

    Ok((
      Self {
        current_value: Arc::new(Mutex::new(None)),
        reloader,
        tx: ReloaderSender { inner: tx },
        config,
        _phantom: std::marker::PhantomData,
      },
      ReloaderReceiver {
        inner: rx,
        _phantom: std::marker::PhantomData,
      },
    ))
  }

  /// Create a new reloader service with default configuration
  pub async fn with_defaults(source: &<T as Reload<V, S>>::Source) -> ReloadResult<(Self, ReloaderReceiver<V, S>), V, S> {
    Self::new(source, ReloaderConfig::default()).await
  }

  /// Create a new reloader service with custom delay
  pub async fn with_delay(
    source: &<T as Reload<V, S>>::Source,
    watch_delay_sec: u32,
  ) -> ReloadResult<(Self, ReloaderReceiver<V, S>), V, S> {
    Self::new(
      source,
      ReloaderConfig {
        watch_delay_sec,
        force_reload: false,
        strategy: WatchStrategy::Polling,
      },
    )
    .await
  }

  /// Start the reloader service watching the target value (polling mode only)
  pub async fn start(&self) -> ReloadResult<(), V, S> {
    match self.config.strategy {
      WatchStrategy::Polling => {
        info!("Starting reloader service in polling mode");
        self.start_polling().await
      }
      WatchStrategy::Realtime | WatchStrategy::Hybrid => {
        error!("Realtime strategies require RealtimeWatch trait. Use start_with_realtime() instead.");
        Err(ReloaderError::Other(anyhow::anyhow!(
          "Realtime strategies require RealtimeWatch implementation. Use start_with_realtime() for types implementing RealtimeWatch."
        )))
      }
    }
  }

  /// Start the service in polling mode
  async fn start_polling(&self) -> ReloadResult<(), V, S> {
    debug!("Polling mode active");

    loop {
      match self.reload_cycle().await {
        Ok(should_continue) => {
          if !should_continue {
            break;
          }
        }
        Err(e) => {
          error!("Critical error in reload cycle: {}", e);
          return Err(e);
        }
      }

      self.sleep_delay().await;
    }

    Ok(())
  }

  /// Start the service in realtime mode
  async fn start_realtime(&self) -> ReloadResult<(), V, S>
  where
    T: RealtimeWatch<V, S>,
  {
    // Load initial value before starting realtime watching
    debug!("Loading initial value before starting realtime mode");
    if !self.execute_reload_cycle("initial value loading").await? {
      return Ok(());
    }

    let mut handle = self.reloader.watch_realtime().await?;
    debug!("Realtime watching established");

    self.run_realtime_loop(&mut handle).await
  }

  /// Start the service in hybrid mode (realtime with polling fallback)
  async fn start_hybrid(&self) -> ReloadResult<(), V, S>
  where
    T: RealtimeWatch<V, S>,
  {
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

  /// Execute a single reload cycle with contextual logging
  async fn execute_reload_cycle(&self, context: &str) -> ReloadResult<bool, V, S> {
    match self.reload_cycle().await {
      Ok(should_continue) => Ok(should_continue),
      Err(e) => {
        error!("Critical error during {}: {}", context, e);
        Err(e)
      }
    }
  }

  /// Continuously process realtime watch events until an error occurs
  async fn run_realtime_loop(&self, handle: &mut RealtimeWatchHandle<V>) -> ReloadResult<(), V, S> {
    loop {
      match handle.rx.recv().await {
        Some(event) => {
          self.handle_watch_event(event).await?;
        }
        None => {
          warn!("Realtime watch channel closed unexpectedly");
          return Err(ReloaderError::Other(anyhow::anyhow!("Realtime watch channel closed")));
        }
      }
    }
  }

  /// Polling fallback that keeps the value fresh until realtime monitoring is available again
  async fn run_polling_fallback(&self) -> ReloadResult<Option<RealtimeWatchHandle<V>>, V, S>
  where
    T: RealtimeWatch<V, S>,
  {
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

  /// Handle a watch event from realtime monitoring
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
        return Err(ReloaderError::Other(anyhow::anyhow!(err)));
      }
    }
    Ok(())
  }

  /// Execute one reload cycle
  async fn reload_cycle(&self) -> ReloadResult<bool, V, S> {
    let target = match self.try_reload().await {
      Ok(Some(target)) => target,
      Ok(None) => {
        warn!("Reloader target was none");
        return Ok(true); // Continue the loop
      }
      Err(e) => {
        warn!("Failed to reload watch target: {}", e);
        return Ok(true); // Continue the loop
      }
    };

    if self.should_broadcast_update(&target).await? {
      self.broadcast_update(target).await?;
    } else {
      debug!("Reloader target was not updated");
    }

    Ok(true)
  }

  /// Attempt to reload the target value
  async fn try_reload(&self) -> ReloadResult<Option<V>, V, S> {
    self.reloader.reload().await
  }

  /// Check if we should broadcast an update for the given target
  async fn should_broadcast_update(&self, target: &V) -> ReloadResult<bool, V, S> {
    if self.config.force_reload {
      let mut current_value = self.current_value.lock().await;
      *current_value = Some(target.clone());
      return Ok(true);
    }

    let mut current_value = self.current_value.lock().await;
    let should_update = match current_value.as_ref() {
      Some(old_value) => old_value != target,
      None => true, // First load
    };

    if should_update {
      *current_value = Some(target.clone());
    }

    Ok(should_update)
  }

  /// Broadcast the updated value to all receivers
  async fn broadcast_update(&self, target: V) -> ReloadResult<(), V, S> {
    info!("Target reloaded. Broadcasting updated value");

    self.tx.send(Some(target)).map_err(ReloaderError::WatchSendError)?;

    Ok(())
  }

  /// Broadcast a removal event to all receivers
  async fn broadcast_removal(&self) -> ReloadResult<(), V, S> {
    info!("Target removed. Broadcasting empty value");

    {
      let mut current_value = self.current_value.lock().await;
      *current_value = None;
    }

    self.tx.send(None).map_err(ReloaderError::WatchSendError)?;

    Ok(())
  }

  /// Sleep for the configured delay period
  async fn sleep_delay(&self) {
    sleep(Duration::from_secs(self.config.watch_delay_sec.into())).await;
  }
}

/// Additional methods for ReloaderService when T implements RealtimeWatch
impl<T, V, S> ReloaderService<T, V, S>
where
  T: RealtimeWatch<V, S> + Clone,
  V: Eq + PartialEq + Clone,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  /// Start the reloader service with realtime support
  ///
  /// This method is available when T implements RealtimeWatch trait.
  /// It respects the configured strategy (Polling or Realtime).
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
}
