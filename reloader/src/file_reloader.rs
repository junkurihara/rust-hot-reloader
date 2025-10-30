use crate::{RealtimeWatch, RealtimeWatchHandle, Reload, ReloaderError, WatchEvent};
use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};
use tokio::{
  runtime::Handle,
  sync::{Mutex, mpsc},
};
use tracing::{debug, error, warn};

/* ---------------------------------------------------------- */
// FileReloader Type

#[derive(Clone)]
pub struct FileReloader<F> {
  pub file_path: PathBuf,
  /// PhantomData is used to mark this type as logically owning F without actually storing it.
  /// This ensures proper variance and drop check behavior for the generic type parameter F.
  _phantom: std::marker::PhantomData<F>,
}

#[async_trait]
impl<F> Reload<F, String> for FileReloader<F>
where
  F: Eq + PartialEq + for<'a> TryFrom<&'a PathBuf> + Sync,
  for<'a> <F as TryFrom<&'a PathBuf>>::Error: std::fmt::Display,
{
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<F, String>> {
    Ok(Self {
      file_path: PathBuf::from(source),
      _phantom: std::marker::PhantomData,
    })
  }

  async fn reload(&self) -> Result<Option<F>, ReloaderError<F, String>> {
    let obj = F::try_from(&self.file_path).map_err(|e| ReloaderError::<F, String>::Reload(e.to_string()))?;
    Ok(Some(obj))
  }
}

/* ---------------------------------------------------------- */
// Realtime File Watching Implementation

/// Default channel size for mpsc channels
const DEFAULT_CHANNEL_SIZE: usize = 100;

/// Duration to debounce rapid successive file events.
/// File system events can fire multiple times for a single logical change
/// (e.g., write + metadata update). This debounce window ensures we only
/// reload once after the events settle.
const FILE_EVENT_DEBOUNCE: Duration = Duration::from_millis(200);

/* ---------------------------------------------------------- */
// Internal Types for Debouncing

#[derive(Debug)]
/// Internal enum to represent debounced file events.
enum DebouncedEvent {
  Reload,
  Removed,
  Error(String),
}

/* ---------------------------------------------------------- */
// AsyncFileLoad Trait

#[async_trait]
/// Trait for loading objects from file paths with async I/O.
/// This trait must be implemented by types that want to be loaded from files.
pub trait AsyncFileLoad: Sized {
  type Error: Send;

  async fn async_load_from<T>(path: T) -> Result<Self, Self::Error>
  where
    T: AsRef<Path> + Send;
}

/* ---------------------------------------------------------- */
// Debouncing Logic

/// Queue and debounce file events to avoid rapid successive reloads.
///
/// Debouncing algorithm:
/// 1. Assign a unique ID to each incoming event using an atomic counter
/// 2. Store the event with its ID in a shared slot
/// 3. Wait for the debounce duration (200ms)
/// 4. After waiting, check if our event ID is still the latest
/// 5. If yes, process the event; if no, a newer event arrived and we discard this one
///
/// Atomic ordering choices:
/// - `fetch_add` uses `AcqRel`: Ensures this operation synchronizes with other threads
/// - `load` uses `Acquire`: Ensures we see the most recent counter value
///
/// This ensures that only the last event in a rapid succession gets processed.
async fn queue_debounced_event<F>(
  event: DebouncedEvent,
  debounce_counter: Arc<AtomicU64>,
  latest_event: Arc<Mutex<Option<(u64, DebouncedEvent)>>>,
  tx: mpsc::Sender<WatchEvent<F>>,
  file_path: PathBuf,
) where
  F: Eq + PartialEq + AsyncFileLoad + Sync,
  <F as AsyncFileLoad>::Error: std::fmt::Display,
{
  // Step 1: Assign a unique ID to this event
  // AcqRel ordering ensures this modification is visible to other threads
  let event_id = debounce_counter.fetch_add(1, Ordering::AcqRel) + 1;

  // Step 2: Store this event as the latest
  {
    let mut slot = latest_event.lock().await;
    *slot = Some((event_id, event));
  }

  // Step 3: Wait for the debounce duration
  tokio::time::sleep(FILE_EVENT_DEBOUNCE).await;

  // Step 4: Check if our event is still the latest
  // Acquire ordering ensures we see the latest counter value
  if debounce_counter.load(Ordering::Acquire) != event_id {
    // A newer event arrived, discard this one
    return;
  }

  // Double-check that our event is still in the slot
  let should_process = {
    let slot = latest_event.lock().await;
    matches!(slot.as_ref(), Some((stored_id, _)) if *stored_id == event_id)
  };

  if !should_process {
    return;
  }

  // Step 5: Extract and process the event
  let event_to_process = {
    let mut slot = latest_event.lock().await;
    slot.take().map(|(_, event)| event)
  };

  if let Some(event) = event_to_process {
    handle_debounced_event(event, &tx, &file_path).await;
  }
}

/// Handle a debounced file event by reading and parsing the file, then sending appropriate events.
async fn handle_debounced_event<F>(
  event: DebouncedEvent,
  tx: &mpsc::Sender<WatchEvent<F>>,
  file_path: &PathBuf,
) where
  F: Eq + PartialEq + AsyncFileLoad + Sync,
  <F as AsyncFileLoad>::Error: std::fmt::Display,
{
  match event {
    DebouncedEvent::Reload => match F::async_load_from(file_path).await {
      Ok(obj) => {
        if let Err(e) = tx.send(WatchEvent::Changed(obj)).await {
          error!("Failed to send changed event: {}", e);
        }
      }
      Err(e) => {
        error!("Failed to read the file: {}", e);
        let message = e.to_string();
        if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
          error!("Failed to send error event: {}", send_err);
        }
      }
    },
    DebouncedEvent::Removed => {
      warn!("The file was removed");
      if let Err(e) = tx.send(WatchEvent::Removed).await {
        error!("Failed to send removed event: {}", e);
      }
    }
    DebouncedEvent::Error(message) => {
      if let Err(e) = tx.send(WatchEvent::Error(message)).await {
        error!("Failed to send error event: {}", e);
      }
    }
  }
}

/* ---------------------------------------------------------- */
// RealtimeWatch Implementation for FileReloader

#[async_trait]
impl<F> RealtimeWatch<F, String> for FileReloader<F>
where
  F: Eq + PartialEq + for<'a> TryFrom<&'a PathBuf> + AsyncFileLoad + Sync + Send + 'static,
  <F as AsyncFileLoad>::Error: std::fmt::Display,
  for<'a> <F as TryFrom<&'a PathBuf>>::Error: std::fmt::Display,
{
  /// Establish a file watcher on the file path.
  /// Returns a handle that receives file change events in real-time.
  async fn watch_realtime(&self) -> Result<RealtimeWatchHandle<F>, ReloaderError<F, String>> {
    let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_SIZE);
    let file_path = self.file_path.clone();
    let debounce_counter = Arc::new(AtomicU64::new(0));
    let latest_event = Arc::new(Mutex::new(None::<(u64, DebouncedEvent)>));

    let watcher = {
      let tx = tx.clone();
      let file_path_for_callback = file_path.clone();
      let debounce_counter = debounce_counter.clone();
      let latest_event = latest_event.clone();

      // Get Tokio runtime handle to spawn tasks from the notify callback thread.
      // The notify library runs callbacks on its own thread pool, but we need to
      // interact with async Tokio code, so we capture the runtime handle here.
      let handle = Handle::current();

      let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let tx = tx.clone();
        let file_path = file_path_for_callback.clone();
        let handle = handle.clone();
        let debounce_counter = debounce_counter.clone();
        let latest_event = latest_event.clone();

        // Spawn async task on Tokio runtime from the notify callback thread.
        // This bridges the gap between notify's sync callback and our async debouncing logic.
        handle.spawn(async move {
          // Classify the file system event
          let event = match res {
            Ok(event) => {
              debug!("File event: {:?}", event);
              match event.kind {
                // File was modified or created - trigger reload
                EventKind::Modify(_) | EventKind::Create(_) => Some(DebouncedEvent::Reload),
                // File was removed - signal removal
                EventKind::Remove(_) => Some(DebouncedEvent::Removed),
                // Ignore other events (access, metadata-only changes, etc.)
                _ => {
                  debug!("Ignoring event kind: {:?}", event.kind);
                  None
                }
              }
            }
            Err(e) => {
              error!("Watch error: {}", e);
              Some(DebouncedEvent::Error(e.to_string()))
            }
          };

          // Queue the event for debouncing
          if let Some(event) = event {
            queue_debounced_event(event, debounce_counter, latest_event, tx, file_path).await;
          }
        });
      })
      .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
        .watch(&file_path, RecursiveMode::NonRecursive)
        .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
    };

    debug!("File watching established for: {:?}", file_path);

    Ok(RealtimeWatchHandle::with_cleanup(rx, Box::new(watcher)))
  }
}
