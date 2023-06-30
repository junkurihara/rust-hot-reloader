use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::{
  sync::watch,
  time::{sleep, Duration},
};
use tracing::{error, info, warn};

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
pub trait ReloaderTarget {
  type TargetValue;
  async fn reload(&self) -> Result<Option<Arc<Self::TargetValue>>, ReloaderError>;
}

pub struct ReloaderSender<T>
where
  T: ReloaderTarget,
{
  inner: watch::Sender<Option<Arc<<T as ReloaderTarget>::TargetValue>>>,
}

#[derive(Clone)]
pub struct ReloaderReceiver<T>
where
  T: ReloaderTarget + Clone,
{
  inner: watch::Receiver<Option<Arc<<T as ReloaderTarget>::TargetValue>>>,
}
impl<T> ReloaderReceiver<T>
where
  T: ReloaderTarget + Clone,
{
  pub async fn changed(&mut self) -> Result<(), ReloaderError> {
    self.inner.changed().await.map_err(ReloaderError::WatchRecvError)
  }

  pub fn borrow(&self) -> watch::Ref<'_, Option<Arc<<T as ReloaderTarget>::TargetValue>>> {
    self.inner.borrow()
  }
}

pub struct ReloaderService<T>
where
  T: ReloaderTarget,
{
  target: T,
  tx: ReloaderSender<T>,
  watch_delay_sec: u32,
}

impl<T> ReloaderService<T>
where
  T: ReloaderTarget + Clone,
{
  pub async fn new(target: T, watch_delay_sec: u32) -> Result<(Self, ReloaderReceiver<T>), ReloaderError> {
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
    loop {
      let Ok(target_opt) = self.target.reload().await else {
        warn!("Failed to reload watch target");
        sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
        continue;
      };
      let Some(target) = target_opt else {
        info!("Target was not updated");
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
