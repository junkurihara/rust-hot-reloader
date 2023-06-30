mod context;

pub use crate::context::{ServerConfig, ServerConfigBuilder, ServerContext, ServerContextBuilder};

use reloader::{Reload, ReloaderService};
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ServerLibError {
  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

pub struct Server {
  pub context: Arc<ServerContext<ServerConfig>>,
}

impl Server {
  pub async fn wait_for_something(&self) -> Result<(), ServerLibError> {
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    Ok(())
  }
  pub async fn entrypoint<T>(&self, context_reloader: ReloaderService<T, ServerConfig>) -> Result<(), ServerLibError>
  where
    T: Reload<ServerConfig> + Clone + Send + Sync + Debug + 'static,
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
        _ = self.wait_for_something() => {
          // do something like after `listener.accept()`
          info!("Current value: {:?}", value);
        }
        // immediately update if watcher detects the change
        _ = rx.changed()  => {
          if rx.borrow().is_none() {
            break;
          }
          value = rx.borrow().clone();
          info!("Received value via watcher");
          info!("value: {:?}", value);
        }
        else => break
      }
    }

    Ok(())
  }
}
