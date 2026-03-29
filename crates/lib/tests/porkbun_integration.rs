use nix_hapi_lib::field_value::ResolvedFieldValue;
use nix_hapi_lib::jq_expr::JqExpr;
use nix_hapi_lib::meta::NixHapiMeta;
use nix_hapi_lib::plan::ResourceChange;
use nix_hapi_lib::provider::{Provider, ResolvedConfig};
use nix_hapi_provider_porkbun_lib::PorkbunProvider;
use std::collections::HashMap;

// ── Fixtures ─────────────────────────────────────────────────────────────────

const DOMAIN: &str = "example.com";

fn make_config(base_url: &str) -> ResolvedConfig {
  HashMap::from([
    ("domain".to_string(), ResolvedFieldValue::Managed(DOMAIN.to_string())),
    ("api_key".to_string(), ResolvedFieldValue::Managed("pk_test".to_string())),
    (
      "secret_api_key".to_string(),
      ResolvedFieldValue::Managed("sk_test".to_string()),
    ),
    ("base_url".to_string(), ResolvedFieldValue::Managed(base_url.to_string())),
  ])
}

fn meta_default() -> NixHapiMeta {
  NixHapiMeta::default()
}

fn meta_with_ignore(patterns: &[&str]) -> NixHapiMeta {
  NixHapiMeta {
    ignore: patterns
      .iter()
      .map(|s| JqExpr::Inline(s.to_string()))
      .collect(),
    ..Default::default()
  }
}

fn empty_live() -> serde_json::Value {
  serde_json::json!({})
}

fn retrieve_body(records: serde_json::Value) -> String {
  serde_json::json!({ "status": "SUCCESS", "records": records }).to_string()
}

fn success_body() -> &'static str {
  r#"{"status":"SUCCESS"}"#
}

fn live_with_a_record(id: &str, content: &str) -> serde_json::Value {
  serde_json::json!({
    "A/www": {
      "id": id,
      "name": format!("www.{DOMAIN}"),
      "type": "A",
      "content": content,
      "ttl": "600",
      "prio": null
    }
  })
}

fn desired_a_managed(content: &str) -> serde_json::Value {
  serde_json::json!({
    "A/www": {
      "content": { "__nixhapi": "managed", "value": content },
      "ttl":     { "__nixhapi": "managed", "value": "600"   }
    }
  })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn creates_missing_record() {
  let mut server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let create_mock = server
    .mock("POST", format!("/dns/create/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create();

  let desired = desired_a_managed("1.2.3.4");
  let live = empty_live();
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert_eq!(plan.changes.len(), 1);
  assert!(
    matches!(&plan.changes[0], ResourceChange::Add { resource_id, .. } if resource_id == "A/www")
  );

  let report = provider
    .apply(&plan, &config)
    .expect("apply should succeed");

  create_mock.assert();
  assert_eq!(report.created, vec!["A/www"]);
  assert!(report.modified.is_empty());
  assert!(report.deleted.is_empty());
}

#[test]
fn deletes_orphan_record() {
  let mut server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let delete_mock = server
    .mock("POST", format!("/dns/delete/{DOMAIN}/42").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create();

  let desired = empty_live();
  let live = live_with_a_record("42", "1.2.3.4");
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert_eq!(plan.changes.len(), 1);
  assert!(
    matches!(&plan.changes[0], ResourceChange::Delete { resource_id } if resource_id == "A/www")
  );

  let report = provider
    .apply(&plan, &config)
    .expect("apply should succeed");

  delete_mock.assert();
  assert!(report.created.is_empty());
  assert!(report.modified.is_empty());
  assert_eq!(report.deleted, vec!["A/www"]);
}

#[test]
fn modifies_changed_record() {
  let mut server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let edit_mock = server
    .mock("POST", format!("/dns/edit/{DOMAIN}/7").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create();

  let desired = desired_a_managed("5.6.7.8");
  let live = live_with_a_record("7", "1.2.3.4");
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert_eq!(plan.changes.len(), 1);
  assert!(
    matches!(&plan.changes[0], ResourceChange::Modify { resource_id, .. } if resource_id == "A/www")
  );

  let report = provider
    .apply(&plan, &config)
    .expect("apply should succeed");

  edit_mock.assert();
  assert!(report.created.is_empty());
  assert_eq!(report.modified, vec!["A/www"]);
  assert!(report.deleted.is_empty());
}

#[test]
fn no_op_when_already_converged() {
  let server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  // No HTTP mocks registered — any HTTP call would fail the test.
  let desired = desired_a_managed("1.2.3.4");
  let live = live_with_a_record("3", "1.2.3.4");
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert!(plan.changes.is_empty(), "expected empty plan");

  let report = provider
    .apply(&plan, &config)
    .expect("apply should succeed");

  assert!(report.created.is_empty());
  assert!(report.modified.is_empty());
  assert!(report.deleted.is_empty());
}

#[test]
fn ignores_exempt_records() {
  let server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  // Live has www and _dkim; desired only tracks www.  _dkim is ignored.
  let live = serde_json::json!({
    "A/www": {
      "id": "1",
      "name": format!("www.{DOMAIN}"),
      "type": "A",
      "content": "1.2.3.4",
      "ttl": "600",
      "prio": null
    },
    "TXT/_dkim": {
      "id": "2",
      "name": format!("_dkim.{DOMAIN}"),
      "type": "TXT",
      "content": "v=DKIM1; ...",
      "ttl": "300",
      "prio": null
    }
  });
  let desired = desired_a_managed("1.2.3.4");
  let meta = meta_with_ignore(&["^TXT/"]);

  let plan = provider
    .plan(&desired, &live, &meta, &config)
    .expect("plan should succeed");

  assert!(
    plan.changes.is_empty(),
    "exempt TXT record should not appear in plan; got: {:?}",
    plan.changes
  );
}

#[test]
fn mkunmanaged_fields_skipped() {
  let server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  // content is Unmanaged — must not be compared even if it differs.
  // ttl is Managed and differs, so we expect one Modify touching only ttl.
  let mut server = server;
  let edit_mock = server
    .mock("POST", format!("/dns/edit/{DOMAIN}/5").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(success_body())
    .create();

  let desired = serde_json::json!({
    "A/www": {
      "content": { "__nixhapi": "unmanaged" },
      "ttl":     { "__nixhapi": "managed", "value": "3600" }
    }
  });
  let live = live_with_a_record("5", "1.2.3.4");
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert_eq!(plan.changes.len(), 1);
  let ResourceChange::Modify { field_changes, .. } = &plan.changes[0] else {
    panic!("expected Modify, got {:?}", plan.changes[0]);
  };
  assert!(
    field_changes.iter().all(|f| f.field != "content"),
    "content (Unmanaged) must not appear in field diffs"
  );
  assert!(
    field_changes.iter().any(|f| f.field == "ttl"),
    "ttl must appear in field diffs"
  );

  provider
    .apply(&plan, &config)
    .expect("apply should succeed");
  edit_mock.assert();
}

#[test]
fn mkinitial_fields_not_overwritten() {
  let server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  // Both fields are Initial and already set in live — no changes expected.
  let desired = serde_json::json!({
    "A/www": {
      "content": { "__nixhapi": "initial", "value": "9.9.9.9" },
      "ttl":     { "__nixhapi": "initial", "value": "9999"   }
    }
  });
  let live = live_with_a_record("6", "1.2.3.4");
  let plan = provider
    .plan(&desired, &live, &meta_default(), &config)
    .expect("plan should succeed");

  assert!(
    plan.changes.is_empty(),
    "Initial fields already in live must not generate changes"
  );
}

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
fn list_live_keys_records_by_type_and_relative_name() {
  let mut server = mockito::Server::new();
  let config = make_config(&server.url());
  let provider = PorkbunProvider;

  let retrieve_mock = server
    .mock("POST", format!("/dns/retrieve/{DOMAIN}").as_str())
    .with_status(200)
    .with_header("content-type", "application/json")
    .with_body(&retrieve_body(serde_json::json!([
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
    .create();

  let live = provider
    .list_live(&config, &[])
    .expect("list_live should succeed");

  retrieve_mock.assert();
  assert!(live.get("A/www").is_some(), "A/www must be present");
  assert!(live.get("MX/@").is_some(), "MX/@ must be present for apex");
}
