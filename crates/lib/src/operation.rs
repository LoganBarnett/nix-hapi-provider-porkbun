// Operation translation: DiffNode (computed by the engine) → PorkbunOperation
// (executable API call).  The engine in nix-hapi-lib owns the diff itself —
// ownership semantics, providerKey matching, rename history, and ignore
// exemptions all live there.  This module just turns the resulting per-node
// changes into Porkbun-shaped operations and back.
//
// ## Identity model
//
// A Porkbun DNS record is identified by the triple `(type, name, content)`.
// Most record types (A, AAAA, MX, NS, TXT, SRV) permit multiple records on
// the same `(type, name)` — round-robin A, multi-MX with different
// priorities, multi-TXT for SPF/DKIM/DMARC/verification — so the triple is
// the smallest tuple that distinguishes records across the supported
// types.  CNAME and SOA are protocol-constrained singletons; the triple is
// still correct for them, just over-specified.
//
// The triple lives in `__nixhapi.providerKey` as a structured object
// `{type, name, content}`.  Changing `content` in desired state is, by
// default, a different identity from the live record — so the engine emits
// `Delete` + `Add`, which the provider executes as two API calls.  Users
// who want an in-place edit instead declare a rename via the providerKey
// chain `[ <new-triple> <old-triple> ]`, and the provider executes the
// match as a single edit-by-id.

use nix_hapi_lib::plan::{DiffNode, FieldDiff, FieldTarget, Status};
use nix_hapi_lib::provider::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// Apex (zone-root) record name on the wire.
pub const APEX: &str = "@";

// Default TTL used when the desired state declares a record without one.
// Matches Porkbun's documented minimum / default.
const DEFAULT_TTL: &str = "600";

/// A live DNS record as we expose it inside the body of a keyed live-state
/// node.  The Porkbun-assigned `id` is what we need at edit/delete time;
/// `type` and `name` are derivable from the providerKey but are surfaced
/// here so the live tree is self-describing for human inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRecord {
  pub id: String,
  /// Full FQDN as returned by Porkbun (e.g. `"www.example.com"`).
  pub name: String,
  #[serde(rename = "type")]
  pub record_type: String,
  pub content: String,
  pub ttl: String,
  #[serde(default)]
  pub prio: Option<String>,
}

/// Wire shape of one operation a runbook step carries.  The provider host
/// serialises it into `RunbookStep.operation` at `build_runbook` time and
/// deserialises it back at `apply` time, so the executor never has to
/// understand the operation surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    /// Kept for human-readable runbook output only.
    content: String,
  },
}

/// Domain-relative record name.  Apex (name == domain) collapses to `"@"`.
pub fn relative_name(fqdn: &str, domain: &str) -> String {
  if fqdn == domain {
    APEX.to_string()
  } else if let Some(rel) = fqdn.strip_suffix(&format!(".{domain}")) {
    rel.to_string()
  } else {
    fqdn.to_string()
  }
}

/// The structured identity of a Porkbun DNS record, mirroring the shape of
/// each `__nixhapi.providerKey` entry.  Used to parse providerKey objects
/// out of a `DiffNode` and to build the canonical live-state lookup key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordIdent {
  pub record_type: String,
  pub name: String,
  /// `None` only when a non-conforming caller emits a providerKey without
  /// `content`.  The Nix module guarantees content is always present for
  /// records it generates; this is permissive for hand-built JSON.
  pub content: Option<String>,
}

impl RecordIdent {
  /// Display string: `"<type>/<name>"` or `"<type>/<name>/<content>"`.
  /// Used in `ApplyReport` entries and runbook descriptions; not a
  /// machine identifier.
  pub fn display(&self) -> String {
    match &self.content {
      Some(c) => format!("{}/{}/{}", self.record_type, self.name, c),
      None => format!("{}/{}", self.record_type, self.name),
    }
  }

  /// Canonical string used as the key in `build_live_index`.  Lookup
  /// keys must match across desired and live sides, so this is built
  /// from the same fields regardless of producer.
  fn lookup_key(&self) -> String {
    self.display()
  }
}

/// Parses one `__nixhapi.providerKey` entry into a `RecordIdent`.
///
/// The canonical shape is the object form `{type, name, content}`;
/// anything else is rejected.
pub fn parse_provider_key_entry(
  entry: &Value,
) -> Result<RecordIdent, ProviderError> {
  let obj = entry.as_object().ok_or_else(|| {
    ProviderError::OperationFailed(format!(
      "Porkbun providerKey entry must be an object \
       {{type, name, content}}, got {entry}"
    ))
  })?;
  let record_type = obj
    .get("type")
    .and_then(Value::as_str)
    .ok_or_else(|| {
      ProviderError::OperationFailed(
        "Porkbun providerKey entry is missing string field \"type\""
          .to_string(),
      )
    })?
    .to_string();
  let name = obj
    .get("name")
    .and_then(Value::as_str)
    .ok_or_else(|| {
      ProviderError::OperationFailed(
        "Porkbun providerKey entry is missing string field \"name\""
          .to_string(),
      )
    })?
    .to_string();
  let content = match obj.get("content") {
    None => None,
    Some(Value::Null) => None,
    Some(Value::String(s)) => Some(s.clone()),
    Some(other) => {
      return Err(ProviderError::OperationFailed(format!(
        "Porkbun providerKey entry \"content\" must be a string or null, \
         got {other}"
      )));
    }
  };
  Ok(RecordIdent {
    record_type,
    name,
    content,
  })
}

/// Walks the live-state forest returned by `list_live` and produces a lookup
/// from canonical `RecordIdent` string to the record body fields we need at
/// build-runbook / apply time.
///
/// The diff engine matches desired↔live by `__nixhapi.providerKey`, so the
/// outer attribute name in the live tree is informational — we index by the
/// canonical providerKey shape instead.
pub fn build_live_index(
  live: &Value,
) -> Result<HashMap<String, LiveRecord>, ProviderError> {
  let map = match live {
    Value::Object(m) => m,
    Value::Null => return Ok(HashMap::new()),
    _ => {
      return Err(ProviderError::LiveStateParse(
        "expected an object at the top of live state".to_string(),
      ));
    }
  };

  map
    .iter()
    .filter_map(|(_, body)| body.as_object().cloned())
    .map(|mut body| -> Result<(String, LiveRecord), ProviderError> {
      let meta = body.remove("__nixhapi").ok_or_else(|| {
        ProviderError::LiveStateParse(
          "live-state record is missing __nixhapi block".to_string(),
        )
      })?;
      let head = meta
        .get("providerKey")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
          ProviderError::LiveStateParse(
            "live-state __nixhapi.providerKey must be a non-empty list"
              .to_string(),
          )
        })?;
      let ident = parse_provider_key_entry(head).map_err(|e| {
        ProviderError::LiveStateParse(format!(
          "live-state providerKey head: {e}"
        ))
      })?;
      let record: LiveRecord = serde_json::from_value(Value::Object(body))
        .map_err(|e| {
          ProviderError::LiveStateParse(format!(
            "live record for providerKey {}: {e}",
            ident.display(),
          ))
        })?;
      Ok((ident.lookup_key(), record))
    })
    .collect()
}

/// Translates a single top-level `DiffNode` into a `PorkbunOperation`.
///
/// Porkbun records are flat — no nested keyed children — so callers do not
/// need to recurse into `node.children`.  If a future migration introduces
/// nesting, this contract should be widened.
pub fn diff_node_to_op(
  node: &DiffNode,
  domain: &str,
  live_index: &HashMap<String, LiveRecord>,
) -> Result<PorkbunOperation, ProviderError> {
  let head = node.provider_key.first().ok_or_else(|| {
    ProviderError::OperationFailed(
      "Porkbun providerKey must be a non-empty list".to_string(),
    )
  })?;
  let head_ident = parse_provider_key_entry(head)?;

  match &node.status {
    Status::Add => {
      let fields = DesiredFields::from_diff(&node.field_changes)?;
      // The providerKey's content is the canonical source of truth for the
      // record's content at Add time; the field diff is allowed to omit
      // content (it was already in the providerKey).  But if it IS in the
      // diff, prefer it — that handles a future world where content is
      // managed independently of identity.
      let content = fields
        .optional_set("content")
        .or_else(|| head_ident.content.clone())
        .ok_or_else(|| {
          ProviderError::OperationFailed(format!(
            "Add of {}: no content available from either the diff or \
             the providerKey",
            head_ident.display()
          ))
        })?;
      let ttl = fields.optional_set("ttl").unwrap_or_else(default_ttl);
      let prio = fields.optional_set("prio");
      Ok(PorkbunOperation::Create {
        domain: domain.to_string(),
        name: head_ident.name.clone(),
        record_type: head_ident.record_type.clone(),
        content,
        ttl,
        prio,
      })
    }

    Status::Modify => {
      let live = lookup_live(&head_ident, live_index)?;
      build_edit_op(domain, &head_ident, live, &node.field_changes)
    }

    Status::Rename { chain } => {
      // chain[0] is the providerKey currently in live state; chain[last]
      // is the new head we want to converge to.  Look up live by the old
      // key, then edit the record so type/name/content match the new key.
      let live_entry = chain.first().ok_or_else(|| {
        ProviderError::OperationFailed(
          "Rename chain must be a non-empty list".to_string(),
        )
      })?;
      let live_ident = parse_provider_key_entry(live_entry)?;
      let live = lookup_live(&live_ident, live_index)?;
      build_edit_op(domain, &head_ident, live, &node.field_changes)
    }

    Status::Delete => {
      let live = lookup_live(&head_ident, live_index)?;
      Ok(PorkbunOperation::Delete {
        domain: domain.to_string(),
        id: live.id.clone(),
        name: head_ident.name.clone(),
        record_type: head_ident.record_type.clone(),
        content: live.content.clone(),
      })
    }
  }
}

fn build_edit_op(
  domain: &str,
  target: &RecordIdent,
  live: &LiveRecord,
  diffs: &[FieldDiff],
) -> Result<PorkbunOperation, ProviderError> {
  let fields = DesiredFields::from_diff(diffs)?;
  // For Edit, the target content is whatever the providerKey carries
  // (the desired identity).  Fall back to a content field diff if the
  // providerKey omits content; finally fall back to the live value so
  // pure TTL/prio modifies don't accidentally clear content.
  let content = target
    .content
    .clone()
    .or_else(|| fields.optional_set("content"))
    .unwrap_or_else(|| live.content.clone());
  let ttl = fields
    .optional_set("ttl")
    .unwrap_or_else(|| live.ttl.clone());
  let prio = if fields.has("prio") {
    fields.optional_set("prio")
  } else {
    live.prio.clone()
  };
  Ok(PorkbunOperation::Edit {
    domain: domain.to_string(),
    id: live.id.clone(),
    name: target.name.clone(),
    record_type: target.record_type.clone(),
    content,
    ttl,
    prio,
  })
}

fn lookup_live<'a>(
  ident: &RecordIdent,
  index: &'a HashMap<String, LiveRecord>,
) -> Result<&'a LiveRecord, ProviderError> {
  index.get(&ident.lookup_key()).ok_or_else(|| {
    ProviderError::OperationFailed(format!(
      "No live Porkbun record found for providerKey {}",
      ident.display(),
    ))
  })
}

fn default_ttl() -> String {
  DEFAULT_TTL.to_string()
}

// ── Desired-field extraction ────────────────────────────────────────────────

/// Field-level summary extracted from a `DiffNode.field_changes`.  Each
/// entry distinguishes "not in the diff at all" (`None`) from "explicitly
/// in the diff" (`Some(target)`), where `target` is `Some(value)` for a
/// set and `None` for a removal.
#[derive(Debug, Default)]
struct DesiredFields {
  content: Option<Option<String>>,
  ttl: Option<Option<String>>,
  prio: Option<Option<String>>,
}

impl DesiredFields {
  fn from_diff(diffs: &[FieldDiff]) -> Result<Self, ProviderError> {
    let mut out = Self::default();
    for diff in diffs {
      let target = match &diff.to {
        FieldTarget::Value { value } => {
          Some(value_to_string(&diff.field, value)?)
        }
        FieldTarget::Removed => None,
        FieldTarget::DerivedPlaceholder { .. } => {
          return Err(ProviderError::OperationFailed(format!(
            "field {:?} carries an unresolved derivedFrom placeholder; \
             Porkbun does not support derivedFrom inputs",
            diff.field
          )));
        }
      };
      match diff.field.as_str() {
        "content" => out.content = Some(target),
        "ttl" => out.ttl = Some(target),
        "prio" => out.prio = Some(target),
        other => {
          return Err(ProviderError::OperationFailed(format!(
            "unsupported Porkbun field {other:?} in diff"
          )));
        }
      }
    }
    Ok(out)
  }

  fn optional_set(&self, field: &str) -> Option<String> {
    let slot = match field {
      "content" => &self.content,
      "ttl" => &self.ttl,
      "prio" => &self.prio,
      _ => return None,
    };
    slot.as_ref().and_then(|inner| inner.clone())
  }

  fn has(&self, field: &str) -> bool {
    match field {
      "content" => self.content.is_some(),
      "ttl" => self.ttl.is_some(),
      "prio" => self.prio.is_some(),
      _ => false,
    }
  }
}

fn value_to_string(
  field: &str,
  value: &Value,
) -> Result<String, ProviderError> {
  match value {
    Value::String(s) => Ok(s.clone()),
    Value::Number(n) => Ok(n.to_string()),
    other => Err(ProviderError::OperationFailed(format!(
      "field {field:?} must be a string, got {other}"
    ))),
  }
}
