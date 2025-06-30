# Periodic hot reloader and notifier for files, KVS, etc. for Rust

[![hot_reload](https://img.shields.io/crates/v/hot_reload.svg)](https://crates.io/crates/hot_reload) [![hot_reload](https://docs.rs/hot_reload/badge.svg)](https://docs.rs/hot_reload) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

This provides a Rust trait definition and service library for hot-reloading your files, KVS, etc. by periodically checking the system.

## `Reload` Trait Definition

To use this library, you need to prepare your own struct implementing `reloader::Reload` trait, defined as follows:

```rust
#[async_trait]
/// Trait defining the responsibility of reloaders to periodically load the target value `V` from `Source`.
/// Source could be a file, a KVS, whatever if you can implement `Reload<V, S>` with `Reload<V, S>::Source`.
/// The generic parameters allow for flexible error handling and value types.
pub trait Reload<V, S = &'static str>
where
  V: Eq + PartialEq,
  S: Into<std::borrow::Cow<'static, str>> + std::fmt::Display,
{
  type Source;
  async fn new(src: &Self::Source) -> ReloadResult<Self, V, S>
  where
    Self: Sized;
  async fn reload(&self) -> ReloadResult<Option<V>, V, S>;
}
```

This trait defines the source type (file, KVS, etc) and reloaded object type `V`. The generic parameter `S` allows for flexible error message types. The following is an example of periodic-reloading a config-file through a given file path string.

```rust
pub struct ConfigReloader {
  pub config_path: PathBuf,
}

#[async_trait]
impl Reload<ServerConfig> for ConfigReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> ReloadResult<Self, ServerConfig> {
    Ok(Self {
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> ReloadResult<Option<ServerConfig>, ServerConfig> {
    let config_str = std::fs::read_to_string(&self.config_path)
      .map_err(|e| ReloaderError::Reload(format!("Failed to read config file: {}", e)))?;
    let config: ServerConfig = config_object_from_str(config_str)
      .map_err(|e| ReloaderError::Reload(format!("Failed to parse config: {}", e)))?;

    Ok(Some(config))
  }
}
```

## Configuration

The `ReloaderConfig` struct allows you to customize the reloader behavior:

```rust
pub struct ReloaderConfig {
  /// Period between reload attempts in seconds
  pub watch_delay_sec: u32,
  /// If true, broadcast updates even when values haven't changed
  pub force_reload: bool,
}
```

The service provides several convenience methods for common configurations:

- `with_defaults()`: Uses default config (10 second delay, no force reload)
- `with_delay(delay_sec)`: Custom delay with no force reload
- `new(source, config)`: Full control over configuration

## Usage

```rust
use hot_reload::*;

// Create reloader service with default configuration (10 second interval)
let (service, mut rx) = ReloaderService::<ConfigReloader, ServerConfig>::with_defaults(&config_path).await.unwrap();

// Or create with custom configuration
let config = ReloaderConfig {
    watch_delay_sec: 5,
    force_reload: false,
};
let (service, mut rx) = ReloaderService::<ConfigReloader, ServerConfig>::new(&config_path, config).await.unwrap();

// Start the reloader service in a background task
tokio::spawn(async move { service.start().await });

// Main event loop
loop {
  tokio::select! {
    // Add main logic of the event loop with up-to-date value
    _ = something.happened() => {
      // ...
    }
    // immediately update if watcher detects the change
    _ = rx.changed() => {
      if let Some(value) = rx.get() {
        info!("Received updated value: {:?}", value);
      } else {
        break; // Service terminated
      }
    }
    else => break
  }
}
```
