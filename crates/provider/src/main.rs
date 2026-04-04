mod logging;

use logging::init_logging;
use nix_hapi_provider_porkbun_lib::PorkbunProvider;

#[tokio::main]
async fn main() {
  init_logging();
  if let Err(e) = nix_hapi_lib::provider_host::run(PorkbunProvider).await {
    eprintln!("nix-hapi-provider-porkbun: {e}");
    std::process::exit(1);
  }
}
