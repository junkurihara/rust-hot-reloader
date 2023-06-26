mod config;
mod error;
mod globals;
mod log;
mod server;
mod watcher;

use config::{parse_opts, ConfigWatch};
use log::*;
use server::Server;

use crate::watcher::{WatcherService, WatcherTarget};

fn main() {
  log::init_logger();
  debug!("Start toml hot-reloading poc");

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name(env!("CARGO_PKG_NAME"));
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    // Setup watcher service
    let watcher_target = ConfigWatch {
      config: config::ConfigToml {
        listen_addresses: None,
        user_info: None,
      },
    };
    let (watcher, rx) = WatcherService::new(watcher_target, 10).await.unwrap();

    // Setup globals that have runtime_handle and rx
    let globals = match parse_opts(runtime.handle(), rx).await {
      Ok(g) => g,
      Err(e) => {
        error!("Failed to parse config TOML: {}", e);
        std::process::exit(1);
      }
    };
    globals.runtime_handle.spawn(async move { watcher.start().await });

    let server = Server { globals };
    server.entrypoint().await.unwrap()
  });
}
