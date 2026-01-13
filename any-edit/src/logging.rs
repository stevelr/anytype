use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_logging(debug: bool, verbose: bool) -> Result<(), anyhow::Error> {
    let level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };

    let _filter = tracing_subscriber::filter::LevelFilter::from_level(level);

    // Only show logs from our crates unless debug is enabled
    let env_filter = if debug {
        EnvFilter::from_default_env()
            .add_directive(format!("api={level}").parse()?)
            .add_directive(format!("cli={level}").parse()?)
    } else {
        EnvFilter::from_default_env()
            .add_directive(format!("api={level}").parse()?)
            .add_directive(format!("cli={level}").parse()?)
            .add_directive("hyper=warn".parse()?)
            .add_directive("reqwest=warn".parse()?)
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(debug)
                .with_thread_ids(debug)
                .with_file(debug)
                .with_line_number(debug),
        )
        .with(env_filter)
        .init();

    Ok(())
}
