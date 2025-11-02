use crate::{RealtimeWatch, RealtimeWatchHandle, Reload, ReloaderError, WatchEvent};
use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::{Mutex, RwLock, mpsc},
};
use tracing::{debug, error, warn};

/* ---------------------------------------------------------- */
// FileReloader Type

pub struct FileReloader<F> {
    /// Path to the main file being watched
    pub file_path: PathBuf,
    /// Set of all tracked file paths (main file + dependencies)
    tracked_paths: Arc<RwLock<HashSet<PathBuf>>>,
    /// PhantomData is used to mark this type as logically owning F without actually storing it.
    /// This ensures proper variance and drop check behavior for the generic type parameter F.
    _phantom: std::marker::PhantomData<F>,
}

impl<F> Clone for FileReloader<F> {
    fn clone(&self) -> Self {
        Self {
            file_path: self.file_path.clone(),
            tracked_paths: self.tracked_paths.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<F> FileReloader<F> {
    fn resolve_dependency_path(&self, path: PathBuf) -> PathBuf {
        let candidate = if path.is_absolute() {
            path
        } else if let Some(parent) = self.file_path.parent() {
            parent.join(path)
        } else {
            path
        };

        candidate.canonicalize().unwrap_or(candidate)
    }

    async fn update_tracked_paths(
        &self,
        dependencies: Vec<PathBuf>,
    ) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut new_set = HashSet::new();
        new_set.insert(self.file_path.clone());
        for dep in dependencies {
            new_set.insert(self.resolve_dependency_path(dep));
        }

        let mut tracked = self.tracked_paths.write().await;
        let added = new_set.difference(&*tracked).cloned().collect::<Vec<_>>();
        let removed = tracked.difference(&new_set).cloned().collect::<Vec<_>>();
        *tracked = new_set;
        (added, removed)
    }

    async fn tracked_paths_snapshot(&self) -> Vec<PathBuf> {
        self.tracked_paths.read().await.iter().cloned().collect()
    }

    fn tracked_paths_handle(&self) -> Arc<RwLock<HashSet<PathBuf>>> {
        self.tracked_paths.clone()
    }
}

#[async_trait]
impl<F> Reload<F, String> for FileReloader<F>
where
    F: Eq + PartialEq + for<'a> TryFrom<&'a PathBuf> + Sync,
    for<'a> <F as TryFrom<&'a PathBuf>>::Error: std::fmt::Display,
{
    type Source = String;
    async fn new(source: &Self::Source) -> Result<Self, ReloaderError<F, String>> {
        let mut file_path = PathBuf::from(source);
        if !file_path.is_absolute() {
            file_path = env::current_dir()
                .map_err(|e| ReloaderError::<F, String>::Other(e.into()))?
                .join(file_path);
        }
        if let Ok(canonical) = file_path.canonicalize() {
            file_path = canonical;
        }

        // By default, track only the main file
        let mut tracked = HashSet::new();
        tracked.insert(file_path.clone());

        Ok(Self {
            file_path,
            tracked_paths: Arc::new(RwLock::new(tracked)),
            _phantom: std::marker::PhantomData,
        })
    }

    async fn reload(&self) -> Result<Option<F>, ReloaderError<F, String>> {
        let obj = F::try_from(&self.file_path)
            .map_err(|e| ReloaderError::<F, String>::Reload(e.to_string()))?;
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
    /// File was removed but needs verification after debounce.
    /// Used for Docker volumes where modifications appear as Remove events.
    RemovedPendingVerification,
    Error(String),
}

/// Shared state for the file watcher to track watched directories.
type SharedWatcherState = Arc<Mutex<Option<Arc<WatcherState>>>>;

/// Tracks the underlying watcher and ensures parent directories are registered.
#[derive(Clone)]
struct WatcherState {
    /// Underlying file system watcher
    watcher: Arc<Mutex<RecommendedWatcher>>,
    /// Set of currently watched directories
    watched_directories: Arc<RwLock<HashSet<PathBuf>>>,
}

impl WatcherState {
    /// Synchronize watched directories with the currently tracked files.
    async fn synchronize_directories(
        &self,
        tracked_files: &[PathBuf],
        added_files: &[PathBuf],
        removed_files: &[PathBuf],
    ) {
        let mut directories_to_add = Vec::new();
        let mut directories_to_remove = Vec::new();

        {
            let mut watched_dirs = self.watched_directories.write().await;

            for path in added_files {
                let parent = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.clone());
                if watched_dirs.insert(parent.clone()) {
                    directories_to_add.push(parent);
                }
            }

            for path in removed_files {
                let parent = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.clone());

                let still_needed = tracked_files.iter().any(|tracked| {
                    tracked
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| tracked.clone())
                        == parent
                });

                if !still_needed && watched_dirs.remove(&parent) {
                    directories_to_remove.push(parent);
                }
            }
        }

        if directories_to_add.is_empty() && directories_to_remove.is_empty() {
            return;
        }

        let mut watcher = self.watcher.lock().await;

        for dir in directories_to_add {
            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                warn!("Failed to watch directory {:?}: {}", dir, e);
            }
        }

        for dir in directories_to_remove {
            if let Err(e) = watcher.unwatch(&dir) {
                warn!("Failed to unwatch directory {:?}: {}", dir, e);
            }
        }
    }
}

fn normalize_event_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let base = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let combined = base.join(path);
        combined.canonicalize().unwrap_or(combined)
    }
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

    /// Return additional file paths that should be watched for changes.
    /// Default implementation returns no extra dependencies.
    fn dependent_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
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
    reloader: Arc<FileReloader<F>>,
    watcher_state: Option<Arc<WatcherState>>,
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
        handle_debounced_event(event, &tx, reloader, watcher_state).await;
    }
}

/// Handle a debounced file event by reading and parsing the file, then sending appropriate events.
async fn handle_debounced_event<F>(
    event: DebouncedEvent,
    tx: &mpsc::Sender<WatchEvent<F>>,
    reloader: Arc<FileReloader<F>>,
    watcher_state: Option<Arc<WatcherState>>,
) where
    F: Eq + PartialEq + AsyncFileLoad + Sync,
    <F as AsyncFileLoad>::Error: std::fmt::Display,
{
    match event {
        DebouncedEvent::Reload => match F::async_load_from(&reloader.file_path).await {
            Ok(obj) => {
                let dependencies = obj.dependent_paths();
                let (added, removed) = reloader.update_tracked_paths(dependencies).await;
                if let Some(state) = watcher_state.clone() {
                    let tracked = reloader.tracked_paths_snapshot().await;
                    state
                        .synchronize_directories(&tracked, &added, &removed)
                        .await;
                }

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
        DebouncedEvent::RemovedPendingVerification => {
            // Check if file still exists after debounce period.
            // Docker volumes may show Remove events for modifications.
            if reloader.file_path.exists() {
                debug!("File was temporarily removed but exists now, treating as modification");
                match F::async_load_from(&reloader.file_path).await {
                    Ok(obj) => {
                        let dependencies = obj.dependent_paths();
                        let (added, removed) = reloader.update_tracked_paths(dependencies).await;
                        if let Some(state) = watcher_state.clone() {
                            let tracked = reloader.tracked_paths_snapshot().await;
                            state
                                .synchronize_directories(&tracked, &added, &removed)
                                .await;
                        }

                        if let Err(e) = tx.send(WatchEvent::Changed(obj)).await {
                            error!("Failed to send changed event: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Failed to read the file after Remove event: {}", e);
                        let message = e.to_string();
                        if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
                            error!("Failed to send error event: {}", send_err);
                        }
                    }
                }
            } else {
                // File actually removed
                warn!("File was removed and does not exist");
                // We do not send WatchEvent::Removed here because the file may be recreated later.
                if let Err(e) = tx.send(WatchEvent::Error("File was removed".into())).await {
                    error!("Failed to send removed event: {}", e);
                }
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
        let tracked_paths = self.tracked_paths_handle();
        let shared_watcher_state: SharedWatcherState = Arc::new(Mutex::new(None));

        let watcher = {
            let tx = tx.clone();
            let debounce_counter = debounce_counter.clone();
            let latest_event = latest_event.clone();
            let tracked_paths = tracked_paths.clone();
            let shared_watcher_state = shared_watcher_state.clone();
            let handle = Handle::current();
            let reloader: Arc<FileReloader<F>> = Arc::new(self.clone());

            notify::recommended_watcher(move |res: notify::Result<Event>| {
                let tx = tx.clone();
                let debounce_counter = debounce_counter.clone();
                let latest_event = latest_event.clone();
                let tracked_paths = tracked_paths.clone();
                let shared_watcher_state = shared_watcher_state.clone();
                let handle = handle.clone();
                let reloader_for_event = reloader.clone();

                handle.spawn(async move {
                    let watcher_state = { shared_watcher_state.lock().await.clone() };

                    let event = match res {
                        Ok(event) => {
                            debug!("File event: {:?}", event);

                            let normalized_paths: Vec<PathBuf> = event
                                .paths
                                .iter()
                                .map(|p| normalize_event_path(p))
                                .collect();
                            let is_relevant = {
                                let tracked = tracked_paths.read().await;
                                normalized_paths.iter().any(|path| tracked.contains(path))
                            };

                            if is_relevant {
                                match event.kind {
                                    // File was modified or created - trigger reload
                                    EventKind::Modify(_) | EventKind::Create(_) => {
                                        Some(DebouncedEvent::Reload)
                                    }
                                    // File was removed - verify after debounce (Docker volume compatibility)
                                    EventKind::Remove(_) => {
                                        Some(DebouncedEvent::RemovedPendingVerification)
                                    }
                                    // Ignore other events (access, metadata-only changes, etc.)
                                    _ => {
                                        debug!("Ignoring event kind: {:?}", event.kind);
                                        None
                                    }
                                }
                            } else {
                                debug!("Ignoring event for unrelated path: {:?}", event.paths);
                                None
                            }
                        }
                        Err(e) => {
                            error!("Watch error: {}", e);
                            Some(DebouncedEvent::Error(e.to_string()))
                        }
                    };

                    if let Some(event) = event {
                        queue_debounced_event(
                            event,
                            debounce_counter,
                            latest_event,
                            tx,
                            reloader_for_event,
                            watcher_state.clone(),
                        )
                        .await;
                    }
                });
            })
            .map_err(|e| ReloaderError::Other(e.into()))?
        };

        let watcher = Arc::new(Mutex::new(watcher));
        let watcher_state = Arc::new(WatcherState {
            watcher: watcher.clone(),
            watched_directories: Arc::new(RwLock::new(HashSet::new())),
        });

        {
            let mut guard = shared_watcher_state.lock().await;
            *guard = Some(watcher_state.clone());
        }

        let (initial_added, initial_removed) = match F::async_load_from(&self.file_path).await {
            Ok(obj) => {
                let deps = obj.dependent_paths();
                self.update_tracked_paths(deps).await
            }
            Err(e) => {
                warn!("Failed to load dependencies during realtime setup: {}", e);
                (Vec::new(), Vec::new())
            }
        };

        let tracked_paths = self.tracked_paths_snapshot().await;

        watcher_state
            .synchronize_directories(&tracked_paths, &initial_added, &initial_removed)
            .await;

        debug!("File watching established for: {:?}", file_path);

        Ok(RealtimeWatchHandle::with_cleanup(
            rx,
            Box::new(watcher_state),
        ))
    }
}
