use tracing_subscriber::{
  fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

// Provider binaries write all log output to stderr so that the JSON-RPC
// protocol on stdout remains unpolluted.
pub fn init_logging() {
  let env_filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"));

  tracing_subscriber::registry()
    .with(
      fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_line_number(true)
        .with_filter(env_filter),
    )
    .init();
}
