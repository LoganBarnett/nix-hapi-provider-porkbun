use crate::client::{PorkbunClient, RecordRequest};
use crate::config::PorkbunConfig;
use crate::operation::{
  build_live_index, diff_node_to_op, parse_provider_key_entry, relative_name,
  LiveRecord, PorkbunOperation, RecordIdent,
};
use async_trait::async_trait;
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{
  ApplyReport, DiffNode, ProviderPlanWave, RunbookStep, Status,
};
use nix_hapi_lib::provider::{Filter, Provider, ProviderError, ResolvedConfig};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::info;

pub struct PorkbunProvider;

#[async_trait]
impl Provider for PorkbunProvider {
  fn provider_type(&self) -> &str {
    "porkbun"
  }

  fn sensitive_config_fields(&self) -> &[&str] {
    &["api_key", "secret_api_key"]
  }

  async fn list_live(
    &self,
    config: &ResolvedConfig,
    _filters: &[Filter],
  ) -> Result<Value, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;
    let client = PorkbunClient::new(
      pb_config.api_key,
      pb_config.secret_api_key,
      pb_config.base_url,
    );

    let records = client.retrieve(&pb_config.domain).await.map_err(|e| {
      ProviderError::ConnectionFailed(format!(
        "Failed to retrieve DNS records for {}: {e}",
        pb_config.domain
      ))
    })?;

    // Emit each live record as a keyed node so the engine can match it
    // against the desired tree by `__nixhapi.providerKey`.  The outer
    // attribute name is informational — the diff engine uses the
    // providerKey value, not the attribute name.
    let live: serde_json::Map<String, Value> = records
      .into_iter()
      .map(|rec| {
        let rel = relative_name(&rec.name, &pb_config.domain);
        let ident = RecordIdent {
          record_type: rec.record_type.clone(),
          name: rel,
          content: Some(rec.content.clone()),
        };
        let live_rec = LiveRecord {
          id: rec.id,
          name: rec.name,
          record_type: rec.record_type,
          content: rec.content,
          ttl: rec.ttl,
          prio: rec.prio,
        };
        let body = live_node_body(&ident, &live_rec);
        (ident.display(), body)
      })
      .collect();

    Ok(Value::Object(live))
  }

  async fn build_runbook(
    &self,
    wave: &ProviderPlanWave,
    _desired: &Value,
    live: &Value,
    _meta: &NixHapiMeta,
    config: &ResolvedConfig,
  ) -> Result<Vec<RunbookStep>, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;
    let live_index = build_live_index(live)?;

    wave
      .changes
      .iter()
      .map(|node| build_step(node, &pb_config.domain, &live_index))
      .collect()
  }

  async fn apply(
    &self,
    wave: &ProviderPlanWave,
    config: &ResolvedConfig,
  ) -> Result<ApplyReport, ProviderError> {
    let pb_config = PorkbunConfig::from_resolved_config(config)?;
    let client = PorkbunClient::new(
      pb_config.api_key,
      pb_config.secret_api_key,
      pb_config.base_url,
    );
    let mut report = ApplyReport::default();

    for step in &wave.runbook {
      let op: PorkbunOperation = serde_json::from_value(step.operation.clone())
        .map_err(|e| {
          ProviderError::OperationFailed(format!(
            "Failed to deserialise operation for {:?}: {e}",
            step.description
          ))
        })?;
      execute_op(&client, op, &mut report).await?;
    }

    Ok(report)
  }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Builds a single `RunbookStep` for one top-level `DiffNode`.  The step's
/// `operation` field carries a serialised `PorkbunOperation` so that `apply`
/// can execute without re-resolving the diff against live state.
fn build_step(
  node: &DiffNode,
  domain: &str,
  live_index: &HashMap<String, LiveRecord>,
) -> Result<RunbookStep, ProviderError> {
  let op = diff_node_to_op(node, domain, live_index)?;
  let (description, command) = describe(node, domain, &op)?;
  let operation = serde_json::to_value(&op).map_err(|e| {
    ProviderError::OperationFailed(format!(
      "Failed to serialise Porkbun operation: {e}"
    ))
  })?;
  Ok(RunbookStep {
    description,
    command,
    body: None,
    operation,
  })
}

fn describe(
  node: &DiffNode,
  domain: &str,
  op: &PorkbunOperation,
) -> Result<(String, String), ProviderError> {
  Ok(match (&node.status, op) {
    (
      Status::Add,
      PorkbunOperation::Create {
        name,
        record_type,
        content,
        ..
      },
    ) => (
      format!("Create {record_type} record {name}.{domain} → {content}"),
      format!("POST /dns/create/{domain}"),
    ),
    (
      Status::Modify,
      PorkbunOperation::Edit {
        id,
        name,
        record_type,
        content,
        ..
      },
    ) => (
      format!("Edit {record_type} record {name}.{domain} (content={content})"),
      format!("POST /dns/edit/{domain}/{id}"),
    ),
    (
      Status::Rename { chain },
      PorkbunOperation::Edit {
        id,
        name,
        record_type,
        content,
        ..
      },
    ) => {
      let from = chain
        .first()
        .map(display_provider_key_entry)
        .transpose()?
        .unwrap_or_else(|| "?".to_string());
      let to = chain
        .last()
        .map(display_provider_key_entry)
        .transpose()?
        .unwrap_or_else(|| "?".to_string());
      (
        format!(
          "Rename {from} → {to} \
           (apply as edit of {record_type} {name}.{domain} → {content})"
        ),
        format!("POST /dns/edit/{domain}/{id}"),
      )
    }
    (
      Status::Delete,
      PorkbunOperation::Delete {
        id,
        name,
        record_type,
        content,
        ..
      },
    ) => (
      format!("Delete {record_type} record {name}.{domain} ({content})"),
      format!("POST /dns/delete/{domain}/{id}"),
    ),
    // Status/op shape mismatch is a bug in diff_node_to_op; surface it
    // visibly rather than silently producing a wrong description.
    (status, op) => (
      format!("Porkbun runbook step ({status:?})"),
      format!("(unrecognised op pairing: {op:?})"),
    ),
  })
}

fn display_provider_key_entry(entry: &Value) -> Result<String, ProviderError> {
  parse_provider_key_entry(entry).map(|ident| ident.display())
}

async fn execute_op(
  client: &PorkbunClient,
  op: PorkbunOperation,
  report: &mut ApplyReport,
) -> Result<(), ProviderError> {
  match op {
    PorkbunOperation::Create {
      domain,
      name,
      record_type,
      content,
      ttl,
      prio,
    } => {
      let key = RecordIdent {
        record_type: record_type.clone(),
        name: name.clone(),
        content: Some(content.clone()),
      }
      .display();
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
        .await
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
      let key = RecordIdent {
        record_type: record_type.clone(),
        name: name.clone(),
        content: Some(content.clone()),
      }
      .display();
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
        .await
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
      content,
    } => {
      let key = RecordIdent {
        record_type,
        name,
        content: Some(content),
      }
      .display();
      info!(key = %key, id = %id, "Deleting DNS record");
      client.delete(&domain, &id).await.map_err(|e| {
        ProviderError::OperationFailed(format!(
          "Failed to delete DNS record {key} (id={id}): {e}"
        ))
      })?;
      report.deleted.push(key);
    }
  }
  Ok(())
}

/// Body of a single live-state keyed node: the structured
/// `__nixhapi.providerKey` marker plus the `LiveRecord` fields flattened
/// into the object.
fn live_node_body(ident: &RecordIdent, rec: &LiveRecord) -> Value {
  let pk_entry = json!({
    "type": ident.record_type,
    "name": ident.name,
    "content": ident.content,
  });
  json!({
    "__nixhapi": { "providerKey": [pk_entry] },
    "id": rec.id,
    "name": rec.name,
    "type": rec.record_type,
    "content": rec.content,
    "ttl": rec.ttl,
    "prio": rec.prio,
  })
}
