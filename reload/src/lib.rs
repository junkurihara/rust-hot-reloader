use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::{
  sync::{watch, Mutex},
  time::{sleep, Duration},
};
use tracing::{debug, error, info, warn};

#[derive(Debug, Error)]
pub enum ReloaderError {
  #[error("Error at watcher receiver")]
  WatchRecvError(#[from] watch::error::RecvError),

  #[error("Failed to reload: {0}")]
  Reload(&'static str),

  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

#[async_trait]
/// Trait defining the responsibility of reloaders to periodically load the target value `V` from `Source`.
/// Source could be a file, a KVS, whatever if you can implement `Reload<V>` with `Reload<V>::Source`.
pub trait Reload<V> {
  type Source;
  async fn new(src: &Self::Source) -> Result<Self, ReloaderError>
  where
    Self: Sized;
  async fn reload(&self) -> Result<Option<V>, ReloaderError>;
}

/// Sender object that simply wraps `tokio::sync:::watch::Sender`
pub struct ReloaderSender<V> {
  inner: watch::Sender<Option<V>>,
}

#[derive(Clone)]
/// Receiver object that simply wraps `tokio::sync:::watch::Receiver`
pub struct ReloaderReceiver<V> {
  inner: watch::Receiver<Option<V>>,
}
impl<V> ReloaderReceiver<V> {
  pub async fn changed(&mut self) -> Result<(), ReloaderError> {
    self.inner.changed().await.map_err(ReloaderError::WatchRecvError)
  }

  pub fn borrow(&self) -> watch::Ref<'_, Option<V>> {
    self.inner.borrow()
  }
}

/// Main object to run reloader service watching the target like config files.
/// This should be spawned as async task like the following
/// ```
/// let (reloader, rx) = ReloaderService::new(source, 10, false).await.unwrap();
/// tokio::spawn(async move { reloader_service.start().await });
/// loop {
///   tokio::select! {
///     // Add main logic of the event loop with up-to-date value
///     _ = something.happened() => {
///       // ...
///     }
///
///     // immediately update if watcher detects the change
///     _ = rx.changed()  => {
///       if rx.borrow().is_none() {
///         break;
///       }
///       let value = rx.borrow().clone();
///       info!("Received value via watcher");
///       info!("value: {:?}", value.unwrap().clone());
///     }
///     else => break
///     }
///   }
/// }
/// ```
pub struct ReloaderService<T, V>
where
  T: Reload<V>,
  V: Eq + PartialEq + Clone,
{
  /// Reloader that responsible to reload the up-to-date value `V`
  reloader: T,

  /// State to manage the current value for checking if the target is updated or not
  current_value: Arc<Mutex<Option<V>>>,

  /// Sender
  tx: ReloaderSender<V>,

  /// Period to reload the target
  watch_delay_sec: u32,

  /// Reload only when the reloader target `V` is updated
  force_reload: bool,
}

impl<T, V> ReloaderService<T, V>
where
  T: Reload<V> + Clone,
  V: Eq + PartialEq + Clone,
{
  /// Instantiate the `ReloaderService<T,V>` object.
  pub async fn new(
    source: &<T as Reload<V>>::Source,
    watch_delay_sec: u32,
    force_reload: bool,
  ) -> Result<(Self, ReloaderReceiver<V>), ReloaderError> {
    let reloader = <T as Reload<V>>::new(source).await?;
    let initial_value = None;
    let (tx, rx) = watch::channel(None);

    Ok((
      Self {
        current_value: Arc::new(Mutex::new(initial_value)),
        reloader,
        tx: ReloaderSender { inner: tx },
        watch_delay_sec,
        force_reload,
      },
      ReloaderReceiver { inner: rx },
    ))
  }

  /// Start the reloader service watching the target value `V`.
  pub async fn start(&self) {
    debug!("Start reloader service");

    loop {
      let Ok(target_opt) = self.reloader.reload().await else {
        warn!("Failed to reload watch target");
        sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
        continue;
      };
      let Some(target) = target_opt else {
        warn!("Reloader target was none");
        sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
        continue;
      };

      if !self.force_reload {
        let mut old_value_opt = self.current_value.lock().await;
        if let Some(old_value) = old_value_opt.clone() {
          if old_value == target {
            debug!("Reloader target was not updated");
            sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
            continue;
          }
        }

        *old_value_opt = Some(target.clone());
      }

      info!("Disseminate up-to-date value");

      if let Err(_e) = self.tx.inner.send(Some(target)) {
        error!("Failed to populate the reloader target");
        break;
      }
      sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
    }
  }
}
