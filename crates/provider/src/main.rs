mod logging;

use logging::init_logging;

fn main() {
  init_logging();
  todo!("Wire up PorkbunProvider via nix_hapi_lib::provider_host::run")
}
