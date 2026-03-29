use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{ApplyReport, ProviderPlan};
use nix_hapi_lib::provider::{Filter, Provider, ProviderError, ResolvedConfig};

pub struct PorkbunProvider;

impl Provider for PorkbunProvider {
  fn provider_type(&self) -> &str {
    "porkbun"
  }

  fn sensitive_config_fields(&self) -> &[&str] {
    &["api_key", "secret_api_key"]
  }

  fn list_live(
    &self,
    _config: &ResolvedConfig,
    _filters: &[Filter],
  ) -> Result<serde_json::Value, ProviderError> {
    Ok(serde_json::json!({}))
  }

  fn plan(
    &self,
    _desired: &serde_json::Value,
    _live: &serde_json::Value,
    _meta: &NixHapiMeta,
    _config: &ResolvedConfig,
  ) -> Result<ProviderPlan, ProviderError> {
    Ok(ProviderPlan {
      instance_name: String::new(),
      provider_type: "porkbun".to_string(),
      changes: vec![],
      runbook: vec![],
    })
  }

  fn apply(
    &self,
    _plan: &ProviderPlan,
    _config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError> {
    Ok(ApplyReport::default())
  }
}
