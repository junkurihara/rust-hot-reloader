mod context;

pub use crate::context::{ServerContext, ServerContextBuilder};

use reload::{ReloaderService, ReloaderTarget};
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ServerLibError {
  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

pub struct Server<T>
where
  T: ReloaderTarget + Clone,
{
  pub context: Arc<ServerContext<T>>,
}

impl<T> Server<T>
where
  T: ReloaderTarget + Clone + Send + Sync + 'static,
{
  pub async fn entrypoint(&self, context_reloader: ReloaderService<T>) -> Result<(), ServerLibError>
  where
    <T as ReloaderTarget>::TargetValue: Debug + Send + Sync,
  {
    // Spawn reloader service
    self
      .context
      .runtime_handle
      .spawn(async move { context_reloader.start().await });

    // Event loop of the main service
    let mut rx = self.context.context_rx.clone();
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
