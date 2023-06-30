mod config;
mod error;
mod log;

use config::parse_opts;
use log::*;
use server_lib::Server;

fn main() {
  log::init_logger();
  debug!("Start toml hot-reloading poc");

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name(env!("CARGO_PKG_NAME"));
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let (reloader, server) = match parse_opts(runtime.handle()).await {
      Ok(all) => all,
      Err(e) => {
        error!("Failed to parse config TOML: {}", e);
        std::process::exit(1);
      }
    };

    server.entrypoint(reloader).await.unwrap()
  });
}
