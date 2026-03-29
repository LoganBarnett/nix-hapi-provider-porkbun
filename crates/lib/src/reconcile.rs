use nix_hapi_lib::dag::eval_jq_first;
use nix_hapi_lib::field_value::FieldValue;
use nix_hapi_lib::jq_expr::JqExpr;
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{FieldDiff, ResourceChange, RunbookStep};
use nix_hapi_lib::provider::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

// The domain-relative record name used for apex records (name == domain).
const APEX: &str = "@";

/// The desired state for a single DNS record.  Each manageable field uses
/// a `FieldValue` so that Managed/Initial/Unmanaged semantics are honoured.
#[derive(Debug, Deserialize)]
pub struct DesiredRecord {
  pub content: FieldValue,
  #[serde(default = "default_ttl")]
  pub ttl: FieldValue,
  #[serde(default)]
  pub prio: Option<FieldValue>,
}

fn default_ttl() -> FieldValue {
  FieldValue::Managed {
    value: "600".to_string(),
  }
}

/// A live DNS record as returned by Porkbun and stored in the live-state blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRecord {
  pub id: String,
  /// Full FQDN as returned by Porkbun (e.g. `"www.example.com"`).
  pub name: String,
  #[serde(rename = "type")]
  pub record_type: String,
  pub content: String,
  pub ttl: String,
  pub prio: Option<String>,
}

/// Encodes a single create, edit, or delete operation for the runbook.
/// Stored in `RunbookStep.operation` and deserialised during `apply`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PorkbunOperation {
  Create {
    domain: String,
    name: String,
    record_type: String,
    content: String,
    ttl: String,
    prio: Option<String>,
  },
  Edit {
    domain: String,
    id: String,
    name: String,
    record_type: String,
    content: String,
    ttl: String,
    prio: Option<String>,
  },
  Delete {
    domain: String,
    id: String,
    /// Kept for human-readable runbook output only.
    name: String,
    record_type: String,
  },
}

/// Converts a Porkbun FQDN into the domain-relative name used as the record
/// key component.  For apex records (name == domain) returns `"@"`.
pub fn relative_name(fqdn: &str, domain: &str) -> String {
  if fqdn == domain {
    APEX.to_string()
  } else if let Some(rel) = fqdn.strip_suffix(&format!(".{domain}")) {
    rel.to_string()
  } else {
    // Unexpected form; use as-is so nothing is silently dropped.
    fqdn.to_string()
  }
}

/// Builds the stable record key used in both the desired and live state maps.
pub fn record_key(record_type: &str, relative: &str) -> String {
  format!("{record_type}/{relative}")
}

/// Parses a record key back into `(type, relative_name)`.
pub fn parse_record_key(key: &str) -> Option<(&str, &str)> {
  key.split_once('/')
}

/// Diffs desired against live and returns resource changes and runbook steps.
///
/// Respects `meta.ignore` patterns: live records whose keys match any pattern
/// are excluded from the delete list, even if absent from desired.
pub fn diff(
  desired: &serde_json::Value,
  live: &serde_json::Value,
  domain: &str,
  meta: &NixHapiMeta,
) -> Result<(Vec<ResourceChange>, Vec<RunbookStep>), ProviderError> {
  let desired_map: HashMap<String, DesiredRecord> =
    serde_json::from_value(desired.clone())
      .map_err(|e| ProviderError::DesiredStateParse(e.to_string()))?;

  let live_map: HashMap<String, LiveRecord> =
    serde_json::from_value(live.clone())
      .map_err(|e| ProviderError::LiveStateParse(e.to_string()))?;

  let ignore_patterns = resolve_ignore_exprs(&meta.ignore)?;

  let mut changes = Vec::new();
  let mut steps = Vec::new();

  // Add and Modify: walk desired.
  for (key, desired_rec) in &desired_map {
    let (rec_type, rel_name) = parse_record_key(key).ok_or_else(|| {
      ProviderError::DesiredStateParse(format!(
        "Record key {key:?} must be in <type>/<name> format"
      ))
    })?;

    match live_map.get(key) {
      None => {
        // Record absent from live — create it.
        let (content, ttl, prio) = resolve_fields_for_create(desired_rec)?;
        let op = PorkbunOperation::Create {
          domain: domain.to_string(),
          name: rel_name.to_string(),
          record_type: rec_type.to_string(),
          content: content.clone(),
          ttl: ttl.clone(),
          prio: prio.clone(),
        };
        changes.push(ResourceChange::Add {
          resource_id: key.clone(),
          fields: add_field_diffs(&content, &ttl, prio.as_deref()),
        });
        steps.push(make_step(
          format!("Create {rec_type} record {rel_name}.{domain}"),
          format!("POST /dns/create/{domain}"),
          &op,
        )?);
      }
      Some(live_rec) => {
        // Record present in live — compare Managed fields.
        let field_changes = managed_field_diffs(desired_rec, live_rec);
        if !field_changes.is_empty() {
          let (content, ttl, prio) =
            resolve_fields_for_edit(desired_rec, live_rec);
          let op = PorkbunOperation::Edit {
            domain: domain.to_string(),
            id: live_rec.id.clone(),
            name: rel_name.to_string(),
            record_type: rec_type.to_string(),
            content,
            ttl,
            prio,
          };
          changes.push(ResourceChange::Modify {
            resource_id: key.clone(),
            field_changes,
          });
          steps.push(make_step(
            format!("Edit {rec_type} record {rel_name}.{domain}"),
            format!("POST /dns/edit/{domain}/{}", live_rec.id),
            &op,
          )?);
        }
      }
    }
  }

  // Destroy: live records absent from desired that are not ignored.
  for (key, live_rec) in &live_map {
    if desired_map.contains_key(key) {
      continue;
    }
    if is_ignored(key, &ignore_patterns) {
      continue;
    }
    let (rec_type, rel_name) = parse_record_key(key)
      .unwrap_or((live_rec.record_type.as_str(), live_rec.name.as_str()));
    let op = PorkbunOperation::Delete {
      domain: domain.to_string(),
      id: live_rec.id.clone(),
      name: rel_name.to_string(),
      record_type: rec_type.to_string(),
    };
    changes.push(ResourceChange::Delete {
      resource_id: key.clone(),
    });
    steps.push(make_step(
      format!("Delete {rec_type} record {rel_name}.{domain}"),
      format!("POST /dns/delete/{domain}/{}", live_rec.id),
      &op,
    )?);
  }

  Ok((changes, steps))
}

/// Resolves Managed/Initial field values for a new record (no live state).
/// Initial fields behave identically to Managed on first creation.
fn resolve_fields_for_create(
  rec: &DesiredRecord,
) -> Result<(String, String, Option<String>), ProviderError> {
  let content = rec
    .content
    .resolve()
    .map_err(|e| ProviderError::OperationFailed(e.to_string()))?
    .value()
    .map(|s| s.to_string())
    .ok_or_else(|| {
      ProviderError::OperationFailed(
        "content field must be Managed or Initial".to_string(),
      )
    })?;

  let ttl = rec
    .ttl
    .resolve()
    .map_err(|e| ProviderError::OperationFailed(e.to_string()))?
    .value()
    .map(|s| s.to_string())
    .unwrap_or_else(|| "600".to_string());

  let prio = rec.prio.as_ref().and_then(|fv| {
    fv.resolve()
      .ok()
      .and_then(|rfv| rfv.value().map(|s| s.to_string()))
  });

  Ok((content, ttl, prio))
}

/// Resolves the effective field values for an Edit operation, respecting
/// Initial semantics: Initial fields are not overwritten if already set.
fn resolve_fields_for_edit(
  desired: &DesiredRecord,
  live: &LiveRecord,
) -> (String, String, Option<String>) {
  let content = effective_value(&desired.content, &live.content);
  let ttl = effective_value(&desired.ttl, &live.ttl);
  let prio = desired.prio.as_ref().map(|fv| {
    let live_prio = live.prio.clone().unwrap_or_default();
    effective_value(fv, &live_prio)
  });
  (content, ttl, prio)
}

/// Returns the desired value unless the field is Initial and live has a
/// non-empty value, in which case the live value is preserved.
fn effective_value(fv: &FieldValue, live: &str) -> String {
  let resolved = match fv.resolve() {
    Ok(r) => r,
    Err(_) => return live.to_string(),
  };
  if resolved.is_initial() && !live.is_empty() {
    return live.to_string();
  }
  resolved
    .value()
    .map(|s| s.to_string())
    .unwrap_or_else(|| live.to_string())
}

/// Produces `FieldDiff` list for Managed fields that differ between desired
/// and live.  Unmanaged and Initial-already-set fields are excluded.
fn managed_field_diffs(
  desired: &DesiredRecord,
  live: &LiveRecord,
) -> Vec<FieldDiff> {
  let mut diffs = Vec::new();

  if let Ok(rfv) = desired.content.resolve() {
    let skip = rfv.is_initial() && !live.content.is_empty();
    if !skip && !rfv.is_unmanaged() {
      if let Some(desired_val) = rfv.value() {
        if desired_val != live.content {
          diffs.push(FieldDiff {
            field: "content".to_string(),
            from: Some(live.content.clone()),
            to: Some(desired_val.to_string()),
          });
        }
      }
    }
  }

  if let Ok(rfv) = desired.ttl.resolve() {
    let skip = rfv.is_initial() && !live.ttl.is_empty();
    if !skip && !rfv.is_unmanaged() {
      if let Some(desired_val) = rfv.value() {
        if desired_val != live.ttl {
          diffs.push(FieldDiff {
            field: "ttl".to_string(),
            from: Some(live.ttl.clone()),
            to: Some(desired_val.to_string()),
          });
        }
      }
    }
  }

  if let Some(prio_fv) = &desired.prio {
    if let Ok(rfv) = prio_fv.resolve() {
      let live_prio = live.prio.clone().unwrap_or_default();
      let skip = rfv.is_initial() && !live_prio.is_empty();
      if !skip && !rfv.is_unmanaged() {
        if let Some(desired_val) = rfv.value() {
          if desired_val != live_prio {
            diffs.push(FieldDiff {
              field: "prio".to_string(),
              from: Some(live_prio),
              to: Some(desired_val.to_string()),
            });
          }
        }
      }
    }
  }

  diffs
}

/// Produces `FieldDiff` list for an Add where all desired fields are new.
fn add_field_diffs(
  content: &str,
  ttl: &str,
  prio: Option<&str>,
) -> Vec<FieldDiff> {
  let mut diffs = vec![
    FieldDiff {
      field: "content".to_string(),
      from: None,
      to: Some(content.to_string()),
    },
    FieldDiff {
      field: "ttl".to_string(),
      from: None,
      to: Some(ttl.to_string()),
    },
  ];
  if let Some(p) = prio {
    diffs.push(FieldDiff {
      field: "prio".to_string(),
      from: None,
      to: Some(p.to_string()),
    });
  }
  diffs
}

fn make_step(
  description: String,
  command: String,
  op: &PorkbunOperation,
) -> Result<RunbookStep, ProviderError> {
  serde_json::to_value(op)
    .map(|operation| RunbookStep {
      description,
      command,
      body: None,
      operation,
    })
    .map_err(|e| ProviderError::OperationFailed(e.to_string()))
}

fn resolve_ignore_exprs(
  exprs: &[JqExpr],
) -> Result<Vec<String>, ProviderError> {
  exprs
    .iter()
    .map(|jq| {
      jq.resolve().map_err(|e| {
        ProviderError::OperationFailed(format!(
          "Failed to resolve ignore expression: {e}"
        ))
      })
    })
    .collect()
}

/// Evaluates ignore expressions against a record key.  Each expression
/// receives `.` as `{"key": "<type>/<name>", "resource_id": "<type>/<name>"}`.
/// A truthy result exempts the record from deletion.
fn is_ignored(key: &str, exprs: &[String]) -> bool {
  exprs.iter().any(|expr| {
    let input = json!({"key": key, "resource_id": key});
    match eval_jq_first("(ignore)", expr, input) {
      Ok(result) => is_truthy(&result),
      Err(_) => false,
    }
  })
}

fn is_truthy(v: &serde_json::Value) -> bool {
  match v {
    serde_json::Value::Null => false,
    serde_json::Value::Bool(b) => *b,
    _ => true,
  }
}
