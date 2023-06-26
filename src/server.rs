use crate::{error::*, globals::Globals, log::*, watcher::WatcherTarget};
use std::sync::Arc;

pub struct Server<T>
where
  T: WatcherTarget + Clone,
{
  pub globals: Arc<Globals<T>>,
}

impl<T> Server<T>
where
  T: WatcherTarget + Clone,
{
  pub async fn entrypoint(&self) -> Result<()>
  where
    <T as WatcherTarget>::TargetValue: std::fmt::Debug,
  {
    // Create channel

    // Event loop of the main service
    let mut rx = self.globals.watcher_rx.clone();
    let mut value = None;
    loop {
      tokio::select! {
        // Add main logic of the event loop with up-to-date value

        // immediately update if watcher detects the change
        _ = rx.changed()  => {
          if rx.borrow().is_none() {
            break;
          }
          value = rx.borrow().clone();
          info!("Received value via watcher");
          info!("value: {:?}", value.unwrap().clone());
        }
        else => break
      }
    }

    Ok(())
  }
}
