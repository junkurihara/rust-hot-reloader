mod context;

pub use crate::context::{ServerConfig, ServerConfigBuilder, ServerContext, ServerContextBuilder};

use anyhow::anyhow;
use hot_reload::{RealtimeWatch, ReloaderService};
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, info};

#[derive(Debug, Error)]
pub enum ServerLibError {
  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct Server<V, S = &'static str>
where
  V: PartialEq + Eq + Clone + Into<ServerConfig> + Debug,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display + Clone,
{
  pub context: Arc<ServerContext<V, S>>,
}

impl<V, S> Server<V, S>
where
  V: PartialEq + Eq + Clone + Into<ServerConfig> + Debug,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display + Clone,
{
  pub async fn wait_for_something(&self) -> Result<(), ServerLibError> {
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    Ok(())
  }
  // pub async fn entrypoint<T>(&self, context_reloader: ReloaderService<T, ServerConfig>) -> Result<(), ServerLibError>
  // where
  //   T: Reload<ServerConfig> + Clone + Send + Sync + Debug + 'static,
  // {
  //   // Exit main thread if either of main service or reloader service exits.
  //   tokio::select! {
  //     // Main service
  //     Err(main_e) = self.main_service() => {
  //       error!("Main service exited: {}", main_e);
  //     },
  //     // Reloader service
  //     Err(reloader_e) = context_reloader.start() => {
  //       error!("Reloader service exited: {}", reloader_e);
  //     },
  //   }

  //   Ok(())
  // }

  pub async fn entrypoint_with_realtime<T>(&self, context_reloader: ReloaderService<T, V, S>) -> Result<(), ServerLibError>
  where
    T: RealtimeWatch<V, S> + Clone + Send + Sync + 'static,
  {
    // Exit main thread if either of main service or reloader service exits.
    tokio::select! {
      // Main service
      Err(main_e) = self.main_service() => {
        error!("Main service exited: {}", main_e);
      },
      // Reloader service with realtime / hybrid support
      Err(reloader_e) = context_reloader.start_with_realtime() => {
        error!("Reloader service exited: {}", reloader_e);
      },
    }

    Ok(())
  }

  async fn main_service(&self) -> Result<(), ServerLibError> {
    let mut rx = self.context.context_rx.clone();
    let mut value = None;
    // Event loop of the main service
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
            // break;
            return Err(ServerLibError::Other(anyhow!("None value")));
          }
          value.clone_from(&rx.borrow());
          info!("Received value via watcher");
          info!("value: {:?}", value);
          let config: ServerConfig = value.clone().unwrap().into();
          info!("turned into server config: {:?}", config);
        }
        else => {
          // break
          return Err(ServerLibError::Other(anyhow!("tokio::secect! else")));
        }
      }
    }
  }
}
