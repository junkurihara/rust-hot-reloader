use derive_builder::Builder;
use reload::ReloaderReceiver;

#[derive(Clone, Builder)]
pub struct ServerContext<ServerConfig> {
  pub(crate) context_rx: ReloaderReceiver<ServerConfig>,
  pub(crate) runtime_handle: tokio::runtime::Handle,
}

#[derive(Clone, Builder, Debug)]
/// Server configuration loaded from file, KVS, wherever through the reloader service.
pub struct ServerConfig {
  name: String,
  id: u32,
}