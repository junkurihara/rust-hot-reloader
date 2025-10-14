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

## Monitoring Strategies

The library supports three monitoring strategies:

1. **Polling** (default): Periodically checks for changes at a configured interval
2. **Realtime**: Uses event-based monitoring (e.g., file system events via `notify` crate)
3. **Hybrid** (recommended): Tries realtime monitoring first, falls back to polling on error and retries realtime

### Configuration

The `ReloaderConfig` struct allows you to customize the reloader behavior:

```rust
pub struct ReloaderConfig {
  /// Period between reload attempts in seconds (used in Polling mode)
  pub watch_delay_sec: u32,
  /// If true, broadcast updates even when values haven't changed
  pub force_reload: bool,
  /// Strategy for watching the target value
  pub strategy: WatchStrategy,
}
```

The service provides several convenience methods:

- `with_defaults()`: Uses default config (10 second polling, no force reload)
- `with_delay(delay_sec)`: Custom delay with polling
- `ReloaderConfig::polling(delay_sec)`: Polling strategy
- `ReloaderConfig::realtime()`: Realtime strategy
- `ReloaderConfig::hybrid(delay_sec)`: Hybrid strategy (recommended)
- `new(source, config)`: Full control over configuration

## Usage

### Basic Usage (Polling Mode)

```rust
use hot_reload::*;

// Create reloader service with default configuration (10 second polling)
let (service, mut rx) = ReloaderService::<ConfigReloader, ServerConfig>::with_defaults(&config_path).await.unwrap();

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

### Realtime Monitoring

To use realtime monitoring, implement the `RealtimeWatch` trait:

```rust
use hot_reload::{Reload, RealtimeWatch, RealtimeWatchHandle, WatchEvent};
use notify::{Watcher, RecommendedWatcher, RecursiveMode};

#[async_trait]
impl RealtimeWatch<ServerConfig> for ConfigReloader {
  async fn watch_realtime(&self) -> ReloadResult<RealtimeWatchHandle<ServerConfig>, ServerConfig> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    let config_path = self.config_path.clone();

    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
      // Handle file system events and send via tx
      // See server-bin/src/config.rs for complete example
    })?;

    watcher.watch(&config_path, RecursiveMode::NonRecursive)?;

    Ok(RealtimeWatchHandle::with_cleanup(rx, Box::new(watcher)))
  }
}
```

Then use the realtime (or hybrid) strategy:

```rust
// Realtime mode
let config = ReloaderConfig::realtime();
let (service, rx) = ReloaderService::new(&config_path, config).await?;

// Or hybrid mode (realtime with automatic polling fallback)
let config = ReloaderConfig::hybrid(10);
let (service, rx) = ReloaderService::new(&config_path, config).await?;

// Use start_with_realtime() for types implementing RealtimeWatch
tokio::spawn(async move { service.start_with_realtime().await });
```

### Strategy Comparison

| Strategy | Latency | Resource Usage | Compatibility | Recommended For |
|----------|---------|----------------|---------------|-----------------|
| **Polling** | Seconds (configurable) | Low | All data sources | KVS, databases, APIs |
| **Realtime** | Milliseconds | Medium | File systems (with `notify`) | File-based configs |
| **Hybrid** | Milliseconds (with fallback) | Medium | File systems (with `notify`) | Production use, resiliency-critical scenarios |

**Note**: The core `hot_reload` library only provides trait definitions. Concrete implementations like file system monitoring with `notify` are demonstrated in the `server-bin` example.

## Example Implementation

This repository includes a complete working example in the `server-bin/` and `server-lib/` directories:

### `server-lib/`

- Provides `Server` struct and `ServerContext` for managing application state
- Demonstrates how to integrate the reloader service into a typical application
- Shows event loop pattern with `tokio::select!` for handling reloaded values
- Supports both polling and realtime monitoring modes via `entrypoint()` and `entrypoint_with_realtime()` methods

### `server-bin/`

- Complete CLI application demonstrating TOML config file hot-reloading
- Implements `RealtimeWatch` trait for `ConfigReloader` using the `notify` crate
- Supports CLI flag `--watch-mode` to choose between `polling`, `realtime`, and `hybrid` strategies
- Default mode is `hybrid`

**Running the example:**

```bash
# Build and run with hybrid mode (default)
cargo run --package server-bin -- --config config.toml

# Run with specific watch mode
cargo run --package server-bin -- --config config.toml --watch-mode realtime|polling|hybrid

# Try modifying config.toml while the server is running to see hot-reloading in action
```

**See also:**

- [server-bin/src/config.rs](server-bin/src/config.rs) - Complete `RealtimeWatch` implementation
- [server-lib/src/lib.rs](server-lib/src/lib.rs) - Integration pattern for applications
