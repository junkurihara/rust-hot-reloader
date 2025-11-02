#[allow(unused)]
pub use tracing::{debug, error, info, warn};

pub fn init_logger() {
    use std::str::FromStr;
    use tracing_subscriber::{fmt, prelude::*};

    let format_layer = fmt::layer()
        .with_line_number(false)
        .with_thread_ids(false)
        .with_target(false)
        .with_thread_names(true)
        .with_target(true)
        .with_level(true)
        .compact();

    // This limits the logger to emits only relevant crate
    let passed_pkg_names = [
        "hot_reload".to_string(),
        "server_bin".to_string(),
        "server_lib".to_string(),
    ];
    let level_string = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let level = tracing::Level::from_str(level_string.as_str()).unwrap_or(tracing::Level::INFO);

    let filter_layer = tracing_subscriber::filter::filter_fn(move |metadata| {
        (passed_pkg_names
            .iter()
            .any(|pkg_name| metadata.target().starts_with(pkg_name))
            && metadata.level() <= &level)
            || metadata.level() <= &tracing::Level::WARN.min(level)
    });

    tracing_subscriber::registry()
        .with(format_layer)
        .with(filter_layer)
        .init();
}
