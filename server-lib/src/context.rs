use derive_builder::Builder;
use reload::{ReloaderReceiver, ReloaderTarget};

#[derive(Clone, Builder)]
pub struct ServerContext<T>
where
  T: ReloaderTarget + Clone,
{
  pub(crate) context_rx: ReloaderReceiver<T>,
  pub(crate) runtime_handle: tokio::runtime::Handle,
}
