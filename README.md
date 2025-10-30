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

- `ReloaderService::with_defaults()`: Uses default config (10 second polling, no force reload)
- `ReloaderService::with_delay(delay_sec)`: Custom delay with polling
- `ReloaderService::new(source, config)`: Full control over configuration

Strategies can be set using:

- `ReloaderConfig::polling(delay_sec)`: Polling strategy
- `ReloaderConfig::realtime()`: Realtime strategy
- `ReloaderConfig::hybrid(delay_sec)`: Hybrid strategy (recommended)

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

**Note**: The core library now includes a built-in `FileReloader` implementation when the `file-reloader` feature is enabled (see below). The `server-bin` example demonstrates additional patterns for custom implementations.

## Built-in FileReloader (Feature: `file-reloader`)

The library provides a ready-to-use `FileReloader` implementation for file-based data sources with built-in realtime monitoring support.

### Enabling the Feature

Add this to your `Cargo.toml`:

```toml
[dependencies]
hot_reload = { version = "0.3", features = ["file-reloader"] }
```

### Using FileReloader

The `FileReloader` is a generic implementation that works with any type implementing the required traits:

```rust
use hot_reload::{FileReloader, ReloaderService, ReloaderConfig, AsyncFileLoad};
use std::path::Path;

// Implement AsyncFileLoad for your config type
#[async_trait]
impl AsyncFileLoad for ServerConfig {
  type Error = anyhow::Error;

  async fn async_load_from<T>(path: T) -> Result<Self, Self::Error>
  where
    T: AsRef<Path> + Send,
  {
    let content = tokio::fs::read_to_string(path).await?;
    let config = toml::from_str(&content)?;
    Ok(config)
  }
}

// Also implement TryFrom<&PathBuf> for synchronous reload support
impl TryFrom<&PathBuf> for ServerConfig {
  type Error = anyhow::Error;

  fn try_from(path: &PathBuf) -> Result<Self, Self::Error> {
    let content = std::fs::read_to_string(path)?;
    let config = toml::from_str(&content)?;
    Ok(config)
  }
}

// Create and use the FileReloader
let config = ReloaderConfig::hybrid(10);
let (service, mut rx) = ReloaderService::<FileReloader<ServerConfig>, ServerConfig, String>::new(
  &config_path,
  config,
).await?;

// Start with realtime monitoring
tokio::spawn(async move { service.start_with_realtime().await });
```

### Features of FileReloader

The `FileReloader` implementation includes:

- **Automatic Debouncing**: File system events are debounced (200ms window) to avoid redundant reloads when multiple events fire for a single logical change
- **Event Filtering**: Only processes relevant events (Create, Modify, Remove) and ignores metadata-only changes
- **Thread-Safe**: Uses atomic operations and proper synchronization for concurrent access
- **Async Integration**: Bridges synchronous `notify` callbacks with async Tokio runtime seamlessly
- **Resource Cleanup**: Automatically manages the file watcher lifecycle via RAII

### How Debouncing Works

The `FileReloader` uses a sophisticated debouncing algorithm:

1. Each file event is assigned a unique ID using an atomic counter
2. Events are stored with their IDs in a shared slot
3. After a 200ms delay, the system checks if the event ID is still the latest
4. Only the most recent event in a rapid succession is processed
5. Older events are automatically discarded

This ensures that even when file system events fire multiple times (e.g., write + metadata update), only one reload occurs.

## Example Implementation

This repository includes a complete working example in the `server-bin/` and `server-lib/` directories:

### `server-lib/`

- Provides `Server` struct and `ServerContext` for managing application state
- Demonstrates how to integrate the reloader service into a typical application
- Shows event loop pattern with `tokio::select!` for handling reloaded values
- Supports both polling and realtime monitoring modes via `entrypoint()` and `entrypoint_with_realtime()` methods

### `server-bin/`

- Complete CLI application demonstrating TOML config file hot-reloading
- Shows custom implementation of `RealtimeWatch` trait for `ConfigReloader` using the `notify` crate
- Supports CLI flag `--watch-mode` to choose between `polling`, `realtime`, and `hybrid` strategies
- Default mode is `hybrid`
- Demonstrates patterns beyond the built-in `FileReloader` for more complex use cases

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
