# Periodic hot reloader and notifier for files, KVS, etc. for Rust

[![hot_reload](https://img.shields.io/crates/v/hot_reload.svg)](https://crates.io/crates/hot_reload) [![hot_reload](https://docs.rs/hot_reload/badge.svg)](https://docs.rs/hot_reload) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

This provides a Rust trait definition and service library for hot-reloading your files, KVS, etc. by periodically checking the system.

## `Reload` Trait Definition

To use this library, you need to prepare your own struct implementing `reloader::Reload` trait, defined as follows:

```rust
#[async_trait]
/// Trait defining the responsibility of reloaders to periodically load the target value `V` from `Source`.
/// Source could be a file, a KVS, whatever if you can implement `Reload<V>` with `Reload<V>::Source`.
pub trait Reload<V>
where
  V: Eq + PartialEq
{
  type Source;
  async fn new(src: &Self::Source) -> Result<Self, ReloaderError>
  where
    Self: Sized;
  async fn reload(&self) -> Result<Option<V>, ReloaderError>;
}
```

This trait defines the source type (file, KVS, etc) and reloaded object type `V`. The following is an example of periodic-reloading a config-file through a given file path string.

```rust
pub struct ConfigReloader {
  pub config_path: PathBuf,
}

#[async_trait]
impl Reload<ServerConfig> for ConfigReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError> {
    Ok(Self {
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> Result<Option<ConfigObject>, ReloaderError> {
    let config_str = std::fs::read_to_string(&self.config_path).context("Failed to read config file")?;
    let config: ConfigObject = config_object_from_str(config_str);

    Ok(Some(config))
  }
}
```

## Usage

```rust
use hot_reload::*;

let (reloader, rx) = ReloaderService::new(source, 10, false).await.unwrap();
tokio::spawn(async move { reloader_service.start().await });
loop {
  tokio::select! {
    // Add main logic of the event loop with up-to-date value
    _ = something.happened() => {
      // ...
    }
    // immediately update if watcher detects the change
    _ = rx.changed()  => {
      if rx.borrow().is_none() {
        break;
      }
      let value = rx.borrow().clone();
      info!("Received value via watcher");
      info!("value: {:?}", value.unwrap().clone());
    }
    else => break
    }
  }
}
```
