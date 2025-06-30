use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::{
  sync::{Mutex, watch},
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

/// Configuration for the reloader service
#[derive(Debug, Clone)]
pub struct ReloaderConfig {
  /// Period between reload attempts in seconds
  pub watch_delay_sec: u32,
  /// If true, broadcast updates even when values haven't changed
  pub force_reload: bool,
}

impl Default for ReloaderConfig {
  fn default() -> Self {
    Self {
      watch_delay_sec: 10,
      force_reload: false,
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
      },
    )
    .await
  }

  /// Start the reloader service watching the target value
  pub async fn start(&self) -> ReloadResult<(), V, S> {
    debug!("Starting reloader service");

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

  /// Sleep for the configured delay period
  async fn sleep_delay(&self) {
    sleep(Duration::from_secs(self.config.watch_delay_sec.into())).await;
  }
}
