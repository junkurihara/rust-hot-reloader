use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::{
  sync::watch,
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
pub trait Reload<V> {
  type Source;
  async fn new(src: &Self::Source) -> Result<Self, ReloaderError>
  where
    Self: Sized;
  async fn reload(&self) -> Result<Option<Arc<V>>, ReloaderError>;
}

pub struct ReloaderSender<V> {
  inner: watch::Sender<Option<Arc<V>>>,
}

#[derive(Clone)]
pub struct ReloaderReceiver<V> {
  inner: watch::Receiver<Option<Arc<V>>>,
}
impl<V> ReloaderReceiver<V> {
  pub async fn changed(&mut self) -> Result<(), ReloaderError> {
    self.inner.changed().await.map_err(ReloaderError::WatchRecvError)
  }

  pub fn borrow(&self) -> watch::Ref<'_, Option<Arc<V>>> {
    self.inner.borrow()
  }
}

pub struct ReloaderService<T, V>
where
  T: Reload<V>,
{
  target: T,
  tx: ReloaderSender<V>,
  watch_delay_sec: u32,
}

impl<T, V> ReloaderService<T, V>
where
  T: Reload<V> + Clone,
{
  pub async fn new(
    source: &<T as Reload<V>>::Source,
    watch_delay_sec: u32,
  ) -> Result<(Self, ReloaderReceiver<V>), ReloaderError> {
    let target = <T as Reload<V>>::new(source).await?;
    let initial_target_value = target.reload().await?;
    let (tx, rx) = watch::channel(initial_target_value);
    Ok((
      Self {
        target,
        tx: ReloaderSender { inner: tx },
        watch_delay_sec,
      },
      ReloaderReceiver { inner: rx },
    ))
  }

  pub async fn start(&self) {
    info!("Start reloader service");
    sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
    loop {
      let Ok(target_opt) = self.target.reload().await else {
        warn!("Failed to reload watch target");
        sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
        continue;
      };
      let Some(target) = target_opt else {
        debug!("Reloader target was not updated");
        sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
        continue;
      };
      if let Err(_e) = self.tx.inner.send(Some(target)) {
        error!("Failed to populate reloader watch target");
        break;
      }
      info!("Target updated. Disseminate updated value.");
      sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
    }
  }
}
