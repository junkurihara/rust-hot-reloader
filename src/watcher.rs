use crate::{error::*, log::*};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::{
  sync::watch,
  time::{sleep, Duration},
};

#[async_trait]
pub trait WatcherTarget {
  type TargetValue;
  async fn reload(&self) -> Result<Option<Arc<Self::TargetValue>>>;
}

pub struct WatcherSender<T>
where
  T: WatcherTarget,
{
  inner: watch::Sender<Option<Arc<<T as WatcherTarget>::TargetValue>>>,
}

#[derive(Clone)]
pub struct WatcherReceiver<T>
where
  T: WatcherTarget + Clone,
{
  inner: watch::Receiver<Option<Arc<<T as WatcherTarget>::TargetValue>>>,
}
impl<T> WatcherReceiver<T>
where
  T: WatcherTarget + Clone,
{
  pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
    self.inner.changed().await
  }

  pub fn borrow(&self) -> watch::Ref<'_, Option<Arc<<T as WatcherTarget>::TargetValue>>> {
    self.inner.borrow()
  }
}

pub struct WatcherService<T>
where
  T: WatcherTarget,
{
  target: T,
  tx: WatcherSender<T>,
  watch_delay_sec: u32,
}

impl<T> WatcherService<T>
where
  T: WatcherTarget + Clone,
{
  pub async fn new(target: T, watch_delay_sec: u32) -> Result<(Self, WatcherReceiver<T>)> {
    let initial_target_value = target.reload().await?;
    let (tx, rx) = watch::channel(initial_target_value);
    Ok((
      Self {
        target,
        tx: WatcherSender { inner: tx },
        watch_delay_sec,
      },
      WatcherReceiver { inner: rx },
    ))
  }

  pub async fn start(&self) {
    info!("Start watcher service");
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
        error!("Failed to populate watch target");
        break;
      }
      info!("Target updated. Disseminate updated value.");
      sleep(Duration::from_secs(self.watch_delay_sec.into())).await;
    }
  }
}
