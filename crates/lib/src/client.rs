use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const BASE_URL: &str = "https://api.porkbun.com/api/json/v3";

#[derive(Debug, Error)]
pub enum PorkbunClientError {
  #[error("HTTP request to Porkbun API failed: {0}")]
  RequestFailed(#[from] reqwest::Error),

  #[error("Porkbun API returned an error for {endpoint}: {message}")]
  ApiError { endpoint: String, message: String },
}

/// Porkbun API credentials included in every request body.
#[derive(Debug, Clone, Serialize)]
struct Auth {
  apikey: String,
  secretapikey: String,
}

/// A single DNS record as returned by the Porkbun retrieve endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PorkbunRecord {
  pub id: String,
  pub name: String,
  #[serde(rename = "type")]
  pub record_type: String,
  pub content: String,
  pub ttl: String,
  pub prio: Option<String>,
  pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RetrieveResponse {
  status: String,
  #[serde(default)]
  message: Option<String>,
  #[serde(default)]
  records: Vec<PorkbunRecord>,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
  status: String,
  #[serde(default)]
  message: Option<String>,
}

/// Thin blocking HTTP client wrapping the Porkbun DNS API.
pub struct PorkbunClient {
  http: Client,
  auth: Auth,
}

impl PorkbunClient {
  pub fn new(api_key: String, secret_api_key: String) -> Self {
    Self {
      http: Client::new(),
      auth: Auth {
        apikey: api_key,
        secretapikey: secret_api_key,
      },
    }
  }

  /// Retrieves all DNS records for `domain`.
  pub fn retrieve(
    &self,
    domain: &str,
  ) -> Result<Vec<PorkbunRecord>, PorkbunClientError> {
    let url = format!("{BASE_URL}/dns/retrieve/{domain}");
    let resp: RetrieveResponse =
      self.http.post(&url).json(&self.auth).send()?.json()?;
    if resp.status != "SUCCESS" {
      return Err(PorkbunClientError::ApiError {
        endpoint: url,
        message: resp.message.unwrap_or_default(),
      });
    }
    Ok(resp.records)
  }

  /// Creates a new DNS record under `domain`.
  pub fn create(
    &self,
    domain: &str,
    record: &RecordRequest,
  ) -> Result<(), PorkbunClientError> {
    let url = format!("{BASE_URL}/dns/create/{domain}");
    let body = AuthedRequest {
      auth: &self.auth,
      record,
    };
    let resp: StatusResponse =
      self.http.post(&url).json(&body).send()?.json()?;
    check_status(resp, url)
  }

  /// Edits an existing DNS record identified by `id` under `domain`.
  pub fn edit(
    &self,
    domain: &str,
    id: &str,
    record: &RecordRequest,
  ) -> Result<(), PorkbunClientError> {
    let url = format!("{BASE_URL}/dns/edit/{domain}/{id}");
    let body = AuthedRequest {
      auth: &self.auth,
      record,
    };
    let resp: StatusResponse =
      self.http.post(&url).json(&body).send()?.json()?;
    check_status(resp, url)
  }

  /// Deletes the DNS record identified by `id` under `domain`.
  pub fn delete(
    &self,
    domain: &str,
    id: &str,
  ) -> Result<(), PorkbunClientError> {
    let url = format!("{BASE_URL}/dns/delete/{domain}/{id}");
    let resp: StatusResponse =
      self.http.post(&url).json(&self.auth).send()?.json()?;
    check_status(resp, url)
  }
}

/// Fields for a create or edit request.
#[derive(Debug, Serialize)]
pub struct RecordRequest {
  pub name: String,
  #[serde(rename = "type")]
  pub record_type: String,
  pub content: String,
  pub ttl: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub prio: Option<String>,
}

/// Serialises auth fields alongside a record request into a single flat
/// object — the format Porkbun expects.
#[derive(Serialize)]
struct AuthedRequest<'a> {
  #[serde(flatten)]
  auth: &'a Auth,
  #[serde(flatten)]
  record: &'a RecordRequest,
}

fn check_status(
  resp: StatusResponse,
  url: String,
) -> Result<(), PorkbunClientError> {
  if resp.status != "SUCCESS" {
    return Err(PorkbunClientError::ApiError {
      endpoint: url,
      message: resp.message.unwrap_or_default(),
    });
  }
  Ok(())
}
