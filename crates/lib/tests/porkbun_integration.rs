// Integration tests for `PorkbunProvider`.  The diff engine in nix-hapi-lib
// is responsible for producing `DiffNode`s from desired/live trees, so these
// tests construct `ProviderPlanWave` fixtures by hand and assert that the
// provider's `build_runbook` and `apply` translate the wave into the expected
// Porkbun API calls.  `list_live` is also covered to ensure the live tree we
// emit matches the keyed-node shape the engine consumes.
//
// ## Identity model under test
//
// A Porkbun record's identity is the triple `(type, name, content)`, encoded
// in `__nixhapi.providerKey` as `[ {type, name, content} ]`.  Tests construct
// providerKey entries with the `pk_entry` helper and report-identifiers with
// `record_id`.

use nix_hapi_lib::field_value::ResolvedFieldValue;
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::{
  DiffNode, FieldDiff, FieldTarget, ProviderPlanWave, Status,
};
use nix_hapi_lib::provider::{Provider, ResolvedConfig};
use nix_hapi_provider_porkbun_lib::operation::PorkbunOperation;
use nix_hapi_provider_porkbun_lib::PorkbunProvider;
use serde_json::{json, Value};
use std::collections::HashMap;

// ── Fixtures ────────────────────────────────────────────────────────────────

const DOMAIN: &str = "example.com";

fn make_config(base_url: &str) -> ResolvedConfig {
  HashMap::from([
    ("domain".to_string(), ResolvedFieldValue::Managed(json!(DOMAIN))),
    ("api_key".to_string(), ResolvedFieldValue::Managed(json!("pk_test"))),
    (
      "secret_api_key".to_string(),
      ResolvedFieldValue::Managed(json!("sk_test")),
    ),
    ("base_url".to_string(), ResolvedFieldValue::Managed(json!(base_url))),
  ])
}

fn success_body() -> &'static str {
  r#"{"status":"SUCCESS"}"#
}

fn retrieve_body(records: Value) -> String {
  json!({ "status": "SUCCESS", "records": records }).to_string()
}

/// A single structured providerKey entry, matching the shape the Nix
/// module emits and the engine compares by deep structural equality.
fn pk_entry(record_type: &str, name: &str, content: &str) -> Value {
  json!({
    "type": record_type,
    "name": name,
    "content": content,
  })
}

/// Display-string form that the provider uses in `ApplyReport` entries
/// and runbook descriptions: `"<type>/<name>/<content>"`.
fn record_id(record_type: &str, name: &str, content: &str) -> String {
  format!("{record_type}/{name}/{content}")
}

/// Shape of a single live keyed-node body, matching what `list_live`
/// produces.  Used both as the `live` argument to `build_runbook` and as
/// reference data when asserting on `list_live` output.
fn live_record_body(
  ident_type: &str,
  ident_name: &str,
  ident_content: &str,
  id: &str,
  full_name: &str,
  ttl: &str,
  prio: Option<&str>,
) -> Value {
  json!({
    "__nixhapi": {
      "providerKey": [pk_entry(ident_type, ident_name, ident_content)],
    },
    "id": id,
    "name": full_name,
    "type": ident_type,
    "content": ident_content,
    "ttl": ttl,
    "prio": prio,
  })
}

fn wave_with(changes: Vec<DiffNode>) -> ProviderPlanWave {
  ProviderPlanWave {
    instance_name: DOMAIN.to_string(),
    provider_type: "porkbun".to_string(),
    wave_index: 0,
    changes,
    runbook: Vec::new(),
  }
}

/// Builds a `DiffNode` keyed by a single providerKey entry (head).  Use
/// `diff_node_with_chain` when the test needs a history list.
fn diff_node(
  head: Value,
  status: Status,
  field_changes: Vec<FieldDiff>,
) -> DiffNode {
  DiffNode {
    provider_key: vec![head],
    path: String::from(".[\"_\"]"),
    status,
    field_changes,
    children: Vec::new(),
  }
}

fn diff_node_with_chain(
  chain: Vec<Value>,
  status: Status,
  field_changes: Vec<FieldDiff>,
) -> DiffNode {
  DiffNode {
    provider_key: chain,
    path: String::from(".[\"_\"]"),
    status,
    field_changes,
    children: Vec::new(),
  }
}

fn set_field(field: &str, value: &str) -> FieldDiff {
  FieldDiff {
    field: field.to_string(),
    from: None,
    to: FieldTarget::Value {
      value: json!(value),
    },
  }
}

fn change_field(field: &str, from: &str, to: &str) -> FieldDiff {
  FieldDiff {
    field: field.to_string(),
    from: Some(json!(from)),
    to: FieldTarget::Value { value: json!(to) },
  }
}

// ── list_live ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_live_emits_keyed_nodes_with_structured_provider_key() {
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let retrieve_mock = server
    .mock("POST", format!("/dns/retrieve/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(retrieve_body(json!([
      {
        "id": "10",
        "name": format!("www.{DOMAIN}"),
        "type": "A",
        "content": "1.2.3.4",
        "ttl": "600",
        "prio": null,
        "notes": ""
      },
      {
        "id": "11",
        "name": DOMAIN,
        "type": "MX",
        "content": format!("mail.{DOMAIN}"),
        "ttl": "3600",
        "prio": "10",
        "notes": ""
      }
    ])))
    .create_async()
    .await;

  let live = provider
    .list_live(&config, &[])
    .await
    .expect("list_live should succeed");

  retrieve_mock.assert_async().await;

  let a_www = live
    .get(record_id("A", "www", "1.2.3.4"))
    .expect("A/www/1.2.3.4 must be present");
  assert_eq!(
    a_www.pointer("/__nixhapi/providerKey/0"),
    Some(&pk_entry("A", "www", "1.2.3.4")),
    "live A/www must carry a structured providerKey object",
  );
  assert_eq!(a_www.get("id").and_then(Value::as_str), Some("10"));
  assert_eq!(a_www.get("content").and_then(Value::as_str), Some("1.2.3.4"));

  let mx_apex = live
    .get(record_id("MX", "@", &format!("mail.{DOMAIN}")))
    .expect("MX apex must be present");
  assert_eq!(
    mx_apex.pointer("/__nixhapi/providerKey/0"),
    Some(&pk_entry("MX", "@", &format!("mail.{DOMAIN}"))),
  );
  assert_eq!(mx_apex.get("prio").and_then(Value::as_str), Some("10"));
}

#[tokio::test]
async fn list_live_indexes_multiple_records_on_same_type_and_name() {
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let retrieve_mock = server
    .mock("POST", format!("/dns/retrieve/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(retrieve_body(json!([
      {
        "id": "10", "name": format!("www.{DOMAIN}"), "type": "A",
        "content": "1.2.3.4", "ttl": "600", "prio": null, "notes": ""
      },
      {
        "id": "11", "name": format!("www.{DOMAIN}"), "type": "A",
        "content": "5.6.7.8", "ttl": "600", "prio": null, "notes": ""
      }
    ])))
    .create_async()
    .await;

  let live = provider
    .list_live(&config, &[])
    .await
    .expect("list_live should succeed");

  retrieve_mock.assert_async().await;
  assert!(live.get(record_id("A", "www", "1.2.3.4")).is_some());
  assert!(live.get(record_id("A", "www", "5.6.7.8")).is_some());
}

// ── build_runbook + apply: Create ───────────────────────────────────────────

#[tokio::test]
async fn add_change_creates_record() {
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let create_mock = server
    .mock("POST", format!("/dns/create/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .match_body(mockito::Matcher::PartialJson(json!({
      "name": "www", "type": "A", "content": "1.2.3.4", "ttl": "600",
    })))
    .create_async()
    .await;

  let wave = wave_with(vec![diff_node(
    pk_entry("A", "www", "1.2.3.4"),
    Status::Add,
    vec![set_field("content", "1.2.3.4"), set_field("ttl", "600")],
  )]);

  let runbook = provider
    .build_runbook(
      &wave,
      &json!({}),
      &json!({}),
      &NixHapiMeta::default(),
      &config,
    )
    .await
    .expect("build_runbook should succeed");

  assert_eq!(runbook.len(), 1);
  let op: PorkbunOperation =
    serde_json::from_value(runbook[0].operation.clone())
      .expect("operation should round-trip");
  assert!(matches!(op, PorkbunOperation::Create { .. }));

  let wave = ProviderPlanWave { runbook, ..wave };
  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  create_mock.assert_async().await;
  assert_eq!(report.created, vec![record_id("A", "www", "1.2.3.4")]);
  assert!(report.modified.is_empty());
  assert!(report.deleted.is_empty());
}

#[tokio::test]
async fn add_change_recovers_content_from_provider_key_when_diff_omits_it() {
  // Engines may legitimately emit an Add with field_changes that omit
  // `content` (the providerKey already carries it).  The provider must
  // fall back to the providerKey's content.
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let create_mock = server
    .mock("POST", format!("/dns/create/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .match_body(mockito::Matcher::PartialJson(json!({
      "name": "@", "type": "TXT", "content": "v=spf1 ~all", "ttl": "600",
    })))
    .create_async()
    .await;

  let wave = wave_with(vec![diff_node(
    pk_entry("TXT", "@", "v=spf1 ~all"),
    Status::Add,
    Vec::new(), // diff lists no fields explicitly
  )]);

  let runbook = provider
    .build_runbook(
      &wave,
      &json!({}),
      &json!({}),
      &NixHapiMeta::default(),
      &config,
    )
    .await
    .expect("build_runbook should succeed");
  let wave = ProviderPlanWave { runbook, ..wave };
  provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  create_mock.assert_async().await;
}

// ── build_runbook + apply: Modify ───────────────────────────────────────────

#[tokio::test]
async fn modify_change_edits_ttl_without_touching_content_identity() {
  // A Modify means the providerKey matched — i.e. content is unchanged.
  // The interesting Modify case is therefore a TTL or prio change.
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let edit_mock = server
    .mock("POST", format!("/dns/edit/{DOMAIN}/7").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .match_body(mockito::Matcher::PartialJson(json!({
      "name": "www", "type": "A", "content": "1.2.3.4", "ttl": "3600",
    })))
    .create_async()
    .await;

  let live = json!({
    record_id("A", "www", "1.2.3.4"): live_record_body(
      "A", "www", "1.2.3.4", "7", &format!("www.{DOMAIN}"), "600", None,
    ),
  });
  let wave = wave_with(vec![diff_node(
    pk_entry("A", "www", "1.2.3.4"),
    Status::Modify,
    vec![change_field("ttl", "600", "3600")],
  )]);

  let runbook = provider
    .build_runbook(&wave, &json!({}), &live, &NixHapiMeta::default(), &config)
    .await
    .expect("build_runbook should succeed");

  let op: PorkbunOperation =
    serde_json::from_value(runbook[0].operation.clone())
      .expect("operation should round-trip");
  let PorkbunOperation::Edit {
    id, ttl, content, ..
  } = &op
  else {
    panic!("expected Edit, got {op:?}");
  };
  assert_eq!(id, "7");
  assert_eq!(ttl, "3600");
  // Content comes from the providerKey identity, not from any field diff
  // (because Modify means the content identity was unchanged).
  assert_eq!(content, "1.2.3.4");

  let wave = ProviderPlanWave { runbook, ..wave };
  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  edit_mock.assert_async().await;
  assert_eq!(report.modified, vec![record_id("A", "www", "1.2.3.4")]);
}

#[tokio::test]
async fn modify_without_live_counterpart_errors() {
  let server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let wave = wave_with(vec![diff_node(
    pk_entry("A", "missing", "1.2.3.4"),
    Status::Modify,
    vec![change_field("ttl", "600", "3600")],
  )]);

  let err = provider
    .build_runbook(
      &wave,
      &json!({}),
      &json!({}),
      &NixHapiMeta::default(),
      &config,
    )
    .await
    .expect_err("build_runbook should error when live lookup misses");

  let msg = err.to_string();
  assert!(
    msg.contains("A/missing/1.2.3.4"),
    "error should name the missing providerKey, got: {msg}"
  );
}

// ── build_runbook + apply: Delete ───────────────────────────────────────────

#[tokio::test]
async fn delete_change_deletes_record() {
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let delete_mock = server
    .mock("POST", format!("/dns/delete/{DOMAIN}/42").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create_async()
    .await;

  let live = json!({
    record_id("A", "www", "1.2.3.4"): live_record_body(
      "A", "www", "1.2.3.4", "42", &format!("www.{DOMAIN}"), "600", None,
    ),
  });
  let wave = wave_with(vec![diff_node(
    pk_entry("A", "www", "1.2.3.4"),
    Status::Delete,
    Vec::new(),
  )]);

  let runbook = provider
    .build_runbook(&wave, &json!({}), &live, &NixHapiMeta::default(), &config)
    .await
    .expect("build_runbook should succeed");
  let wave = ProviderPlanWave { runbook, ..wave };
  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  delete_mock.assert_async().await;
  assert_eq!(report.deleted, vec![record_id("A", "www", "1.2.3.4")]);
}

// ── build_runbook + apply: Rename ───────────────────────────────────────────

#[tokio::test]
async fn rename_change_edits_record_in_place() {
  // User declares the rename chain so a content change executes as a
  // single Porkbun edit-by-id instead of a delete+create pair.
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let edit_mock = server
    .mock("POST", format!("/dns/edit/{DOMAIN}/99").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .match_body(mockito::Matcher::PartialJson(json!({
      "name": "www", "type": "A", "content": "5.6.7.8",
    })))
    .create_async()
    .await;

  let live = json!({
    record_id("A", "www", "1.2.3.4"): live_record_body(
      "A", "www", "1.2.3.4", "99", &format!("www.{DOMAIN}"), "600", None,
    ),
  });
  let new_head = pk_entry("A", "www", "5.6.7.8");
  let old_head = pk_entry("A", "www", "1.2.3.4");
  let node = diff_node_with_chain(
    vec![new_head.clone(), old_head.clone()],
    Status::Rename {
      chain: vec![old_head, new_head],
    },
    Vec::new(),
  );
  let wave = wave_with(vec![node]);

  let runbook = provider
    .build_runbook(&wave, &json!({}), &live, &NixHapiMeta::default(), &config)
    .await
    .expect("build_runbook should succeed");

  let op: PorkbunOperation =
    serde_json::from_value(runbook[0].operation.clone())
      .expect("operation should round-trip");
  let PorkbunOperation::Edit {
    id,
    name,
    record_type,
    content,
    ..
  } = &op
  else {
    panic!("expected Edit, got {op:?}");
  };
  assert_eq!(id, "99");
  assert_eq!(name, "www");
  assert_eq!(record_type, "A");
  assert_eq!(content, "5.6.7.8");

  let wave = ProviderPlanWave { runbook, ..wave };
  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  edit_mock.assert_async().await;
  assert_eq!(report.modified, vec![record_id("A", "www", "5.6.7.8")]);
}

// ── Multi-record on same (type, name) ───────────────────────────────────────

#[tokio::test]
async fn delete_one_of_two_records_on_same_type_and_name() {
  // Two A records on the same name; desired keeps one and drops the
  // other.  Each record is identified independently by its content.
  let mut server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let delete_mock = server
    .mock("POST", format!("/dns/delete/{DOMAIN}/11").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create_async()
    .await;

  let live = json!({
    record_id("A", "www", "1.2.3.4"): live_record_body(
      "A", "www", "1.2.3.4", "10", &format!("www.{DOMAIN}"), "600", None,
    ),
    record_id("A", "www", "5.6.7.8"): live_record_body(
      "A", "www", "5.6.7.8", "11", &format!("www.{DOMAIN}"), "600", None,
    ),
  });
  let wave = wave_with(vec![diff_node(
    pk_entry("A", "www", "5.6.7.8"),
    Status::Delete,
    Vec::new(),
  )]);

  let runbook = provider
    .build_runbook(&wave, &json!({}), &live, &NixHapiMeta::default(), &config)
    .await
    .expect("build_runbook should succeed");
  let wave = ProviderPlanWave { runbook, ..wave };
  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");

  delete_mock.assert_async().await;
  assert_eq!(report.deleted, vec![record_id("A", "www", "5.6.7.8")]);
}

// ── Empty wave ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_wave_produces_no_runbook_and_no_calls() {
  let server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  // No HTTP mocks — any outbound call would fail the test.
  let wave = wave_with(Vec::new());

  let runbook = provider
    .build_runbook(
      &wave,
      &json!({}),
      &json!({}),
      &NixHapiMeta::default(),
      &config,
    )
    .await
    .expect("build_runbook should succeed");
  assert!(runbook.is_empty());

  let report = provider
    .apply(&wave, &config)
    .await
    .expect("apply should succeed");
  assert!(report.created.is_empty());
  assert!(report.modified.is_empty());
  assert!(report.deleted.is_empty());

  drop(server);
}

// ── Provider-static surface ─────────────────────────────────────────────────

#[test]
fn sensitive_fields_declared() {
  let provider = PorkbunProvider;
  let fields = provider.sensitive_config_fields();
  assert!(fields.contains(&"api_key"), "api_key must be sensitive");
  assert!(
    fields.contains(&"secret_api_key"),
    "secret_api_key must be sensitive"
  );
}

#[test]
fn provider_type_is_porkbun() {
  let provider = PorkbunProvider;
  assert_eq!(provider.provider_type(), "porkbun");
}

// ── DerivedPlaceholder rejection ────────────────────────────────────────────

#[tokio::test]
async fn derived_placeholder_in_diff_is_rejected() {
  let server = mockito::Server::new_async().await;
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let mut inputs = std::collections::BTreeMap::new();
  inputs.insert("upstream".to_string(), ".[\"other\"][\"v\"]".to_string());
  let placeholder = FieldDiff {
    field: "content".to_string(),
    from: None,
    to: FieldTarget::DerivedPlaceholder { inputs },
  };
  let wave = wave_with(vec![diff_node(
    pk_entry("A", "www", "1.2.3.4"),
    Status::Add,
    vec![placeholder],
  )]);

  let err = provider
    .build_runbook(
      &wave,
      &json!({}),
      &json!({}),
      &NixHapiMeta::default(),
      &config,
    )
    .await
    .expect_err("build_runbook must reject derivedFrom placeholders");
  let msg = err.to_string();
  assert!(
    msg.contains("derivedFrom"),
    "error message should mention derivedFrom, got: {msg}"
  );

  drop(server);
}
