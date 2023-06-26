use crate::watcher::{WatcherReceiver, WatcherTarget};

#[derive(Clone)]
pub struct Globals<T>
where
  T: WatcherTarget + Clone,
{
  pub config_path: String,
  pub watcher_rx: WatcherReceiver<T>,
  pub runtime_handle: tokio::runtime::Handle,
}
