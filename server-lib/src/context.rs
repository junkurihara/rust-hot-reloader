use derive_builder::Builder;
use hot_reload::ReloaderReceiver;

#[derive(Clone, Builder)]
pub struct ServerContext<V>
where
  V: Eq + PartialEq,
{
  pub(crate) context_rx: ReloaderReceiver<V>,
  pub(crate) runtime_handle: tokio::runtime::Handle,
}

#[derive(Clone, Builder, Debug, PartialEq, Eq)]
/// Server configuration loaded from file, KVS, wherever through the reloader service.
pub struct ServerConfig {
  name: String,
  id: u32,
}
