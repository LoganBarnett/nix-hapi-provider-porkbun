use crate::client::PRODUCTION_BASE_URL;
use nix_hapi_lib::provider::{ProviderError, ResolvedConfig};

/// Resolved provider configuration fields required to talk to Porkbun.
pub struct PorkbunConfig {
  pub domain: String,
  pub api_key: String,
  pub secret_api_key: String,
  /// API base URL.  Defaults to the Porkbun production endpoint; override in
  /// tests to point at a mock server.
  pub base_url: String,
}

impl PorkbunConfig {
  pub fn from_resolved_config(
    config: &ResolvedConfig,
  ) -> Result<Self, ProviderError> {
    let domain = required_field(config, "domain")?;
    let api_key = required_field(config, "api_key")?;
    let secret_api_key = required_field(config, "secret_api_key")?;
    let base_url = optional_field(config, "base_url")
      .unwrap_or_else(|| PRODUCTION_BASE_URL.to_string());
    Ok(Self {
      domain,
      api_key,
      secret_api_key,
      base_url,
    })
  }
}

fn optional_field(config: &ResolvedConfig, field: &str) -> Option<String> {
  config
    .get(field)
    .and_then(|fv| fv.value().map(|s| s.to_string()))
}

fn required_field(
  config: &ResolvedConfig,
  field: &str,
) -> Result<String, ProviderError> {
  config
    .get(field)
    .ok_or_else(|| ProviderError::MissingConfig {
      field: field.to_string(),
    })
    .and_then(|fv| {
      fv.value().map(|s| s.to_string()).ok_or_else(|| {
        ProviderError::UnmanagedConfig {
          field: field.to_string(),
        }
      })
    })
}
