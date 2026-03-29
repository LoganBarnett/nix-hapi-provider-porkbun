use crate::client::{PorkbunClient, RecordRequest};
use crate::config::PorkbunConfig;
use crate::reconcile::{
  record_key, relative_name, LiveRecord, PorkbunOperation,
};
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{ApplyReport, ProviderPlan};
use nix_hapi_lib::provider::{Filter, Provider, ProviderError, ResolvedConfig};
use tracing::info;

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
    config: &ResolvedConfig,
    _filters: &[Filter],
  ) -> Result<serde_json::Value, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;
    let client =
      PorkbunClient::new(pb_config.api_key, pb_config.secret_api_key);

    let records = client.retrieve(&pb_config.domain).map_err(|e| {
      ProviderError::ConnectionFailed(format!(
        "Failed to retrieve DNS records for {}: {e}",
        pb_config.domain
      ))
    })?;

    let live: serde_json::Map<String, serde_json::Value> = records
      .into_iter()
      .map(|rec| {
        let rel = relative_name(&rec.name, &pb_config.domain);
        let key = record_key(&rec.record_type, &rel);
        let live_rec = LiveRecord {
          id: rec.id,
          name: rec.name,
          record_type: rec.record_type,
          content: rec.content,
          ttl: rec.ttl,
          prio: rec.prio,
        };
        let value = serde_json::to_value(live_rec)
          .expect("LiveRecord serialisation is infallible");
        (key, value)
      })
      .collect();

    Ok(serde_json::Value::Object(live))
  }

  fn plan(
    &self,
    desired: &serde_json::Value,
    live: &serde_json::Value,
    meta: &NixHapiMeta,
    config: &ResolvedConfig,
  ) -> Result<ProviderPlan, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;

    let (changes, runbook) =
      crate::reconcile::diff(desired, live, &pb_config.domain, meta)?;

    Ok(ProviderPlan {
      instance_name: String::new(),
      provider_type: self.provider_type().to_string(),
      changes,
      runbook,
    })
  }

  fn apply(
    &self,
    plan: &ProviderPlan,
    config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;
    let client =
      PorkbunClient::new(pb_config.api_key, pb_config.secret_api_key);
    let mut report = ApplyReport::default();

    for step in &plan.runbook {
      let op: PorkbunOperation = serde_json::from_value(step.operation.clone())
        .map_err(|e| {
          ProviderError::OperationFailed(format!(
            "Failed to deserialise operation for {:?}: {e}",
            step.description
          ))
        })?;

      match op {
        PorkbunOperation::Create {
          domain,
          name,
          record_type,
          content,
          ttl,
          prio,
        } => {
          let key = format!("{record_type}/{name}");
          info!(key = %key, "Creating DNS record");
          client
            .create(
              &domain,
              &RecordRequest {
                name,
                record_type,
                content,
                ttl,
                prio,
              },
            )
            .map_err(|e| {
              ProviderError::OperationFailed(format!(
                "Failed to create DNS record {key}: {e}"
              ))
            })?;
          report.created.push(key);
        }

        PorkbunOperation::Edit {
          domain,
          id,
          name,
          record_type,
          content,
          ttl,
          prio,
        } => {
          let key = format!("{record_type}/{name}");
          info!(key = %key, id = %id, "Editing DNS record");
          client
            .edit(
              &domain,
              &id,
              &RecordRequest {
                name,
                record_type,
                content,
                ttl,
                prio,
              },
            )
            .map_err(|e| {
              ProviderError::OperationFailed(format!(
                "Failed to edit DNS record {key} (id={id}): {e}"
              ))
            })?;
          report.modified.push(key);
        }

        PorkbunOperation::Delete {
          domain,
          id,
          name,
          record_type,
        } => {
          let key = format!("{record_type}/{name}");
          info!(key = %key, id = %id, "Deleting DNS record");
          client.delete(&domain, &id).map_err(|e| {
            ProviderError::OperationFailed(format!(
              "Failed to delete DNS record {key} (id={id}): {e}"
            ))
          })?;
          report.deleted.push(key);
        }
      }
    }

    Ok(report)
  }
}
