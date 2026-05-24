//! HTTP client for shim.
//!
//! Strong-typed wrappers around the contract documented in
//! `shim/API.md`. The Rust app should never construct raw
//! shim URLs outside this module — all changes to the contract
//! flow through these types.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountSummary {
    pub account_id: String,
    pub locale: Option<String>,
    pub email_masked: String,
    pub customer_name: Option<String>,
    pub expires_at: Option<i64>,
    pub needs_refresh: bool,
    pub needs_relogin: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LibraryBook {
    pub asin: String,
    pub title: String,
    pub subtitle: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub series: Vec<SeriesEntry>,
    pub runtime_length_min: Option<u64>,
    pub release_date: Option<String>,
    pub publisher_name: Option<String>,
    pub language: Option<String>,
    pub cover_url: Option<String>,
    pub is_aaxc: bool,
    pub is_aax: bool,
    pub is_listenable: bool,
    pub purchase_date: Option<String>,
    pub status: String,
    pub content_delivery_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SeriesEntry {
    pub title: Option<String>,
    pub sequence: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LibraryPage {
    pub total_results: u64,
    pub page: u64,
    pub items: Vec<LibraryBook>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VoucherResp {
    pub asin: String,
    pub content_url: String,
    pub codec: String,
    pub key: String,
    pub iv: String,
    #[serde(default)]
    pub chapters: Vec<ChapterEntry>,
    pub runtime_length_ms: u64,
    pub cover_url: Option<String>,
    pub refresh_date: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChapterEntry {
    pub title: Option<String>,
    pub start_offset_ms: u64,
    pub length_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct LoginStartReq<'a> {
    pub locale: &'a str,
    pub with_username: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoginStartResp {
    pub session_id: String,
    pub open_url: String,
    pub instructions: String,
}

#[derive(Debug, Serialize)]
pub struct LoginFinishReq<'a> {
    pub session_id: &'a str,
    pub redirect_url: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LoginFinishResp {
    pub account_id: String,
    pub customer_name: Option<String>,
    pub locale: Option<String>,
}

pub struct ShimClient<'a> {
    state: &'a AppState,
}

impl<'a> ShimClient<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.state.cfg.shim_url.trim_end_matches('/'), path)
    }

    pub async fn health(&self) -> bool {
        matches!(
            self.state.http.get(self.url("/health")).send().await,
            Ok(r) if r.status().is_success()
        )
    }

    pub async fn list_accounts(&self) -> Result<Vec<AccountSummary>, AppError> {
        let r = self.state.http.get(self.url("/accounts")).send().await?;
        Ok(r.error_for_status()?.json().await?)
    }

    pub async fn login_start(&self, req: LoginStartReq<'_>) -> Result<LoginStartResp, AppError> {
        let r = self
            .state
            .http
            .post(self.url("/login/start"))
            .json(&req)
            .send()
            .await?;
        Ok(r.error_for_status()?.json().await?)
    }

    pub async fn login_finish(&self, req: LoginFinishReq<'_>) -> Result<LoginFinishResp, AppError> {
        let r = self
            .state
            .http
            .post(self.url("/login/finish"))
            .json(&req)
            .send()
            .await?;
        Ok(r.error_for_status()?.json().await?)
    }

    pub async fn library(
        &self,
        account_id: &str,
        page: u64,
        num_results: u64,
        status: Option<&str>,
    ) -> Result<LibraryPage, AppError> {
        let mut req = self
            .state
            .http
            .get(self.url(&format!("/accounts/{account_id}/library")))
            .query(&[("page", page), ("num_results", num_results)]);
        if let Some(s) = status {
            req = req.query(&[("status", s)]);
        }
        Ok(req.send().await?.error_for_status()?.json().await?)
    }

    pub async fn voucher(&self, account_id: &str, asin: &str) -> Result<VoucherResp, AppError> {
        let r = self
            .state
            .http
            .get(self.url(&format!("/accounts/{account_id}/books/{asin}/voucher")))
            .send()
            .await?;
        // 410 = Audible refused to license this ASIN to this customer
        // (Plus catalog rotation, cross-region denial). Distinct from a
        // generic 5xx so the queue can skip retries and the UI can
        // label it as "license denied" rather than "failed".
        if r.status() == reqwest::StatusCode::GONE {
            let detail = r
                .text()
                .await
                .unwrap_or_else(|_| "license denied".into());
            return Err(AppError::LicenseDenied(detail));
        }
        Ok(r.error_for_status()?.json().await?)
    }
}
