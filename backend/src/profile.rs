//! Per-user profile model.
//!
//! Each authenticated user (OIDC sub or DEV_AUTH cookie) maps to exactly
//! one profile row. Audible accounts hang off `accounts.profile_id`,
//! library books + jobs are scoped via that FK.
//!
//! kanidm is the bouncer — if a request reaches `resolve_or_create`,
//! the user is allowed in. First contact (no profile yet for this sub)
//! auto-creates one. No role distinction; no admin gate. The single-
//! library v1 ties output paths to env vars; v2 will move them to a
//! `libraries` table with per-profile binding.

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub id: i64,
    pub user_sub: Option<String>,
    pub email: String,
    pub created_at: i64,
}

/// Resolve (or create) the profile for an authenticated request.
///
/// `email` defaults to `{sub}@local` in callers when the upstream identity
/// provider doesn't surface a real email (e.g. DEV_AUTH without ?email=).
pub async fn resolve_or_create(state: &AppState, sub: &str, email: &str) -> Result<Profile, AppError> {
    let sub_s = sub.to_string();
    let email_s = email.to_lowercase();

    if let Some(p) = lookup_by_sub(state, &sub_s).await? {
        return Ok(p);
    }
    create(state, &sub_s, &email_s).await
}

async fn lookup_by_sub(state: &AppState, sub: &str) -> Result<Option<Profile>, AppError> {
    let sub_s = sub.to_string();
    let row = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT id, user_sub, email, created_at FROM profile WHERE user_sub = ?1",
                rusqlite::params![sub_s],
                row_to_profile,
            )
            .optional()
        })
        .await?;
    Ok(row)
}

async fn create(state: &AppState, sub: &str, email: &str) -> Result<Profile, AppError> {
    let sub_s = sub.to_string();
    let email_s = email.to_string();
    let now = Utc::now().timestamp();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO profile (user_sub, email, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![sub_s, email_s, now],
            )?;
            Ok(())
        })
        .await?;
    Ok(lookup_by_sub(state, sub).await?.expect("just inserted"))
}

fn row_to_profile(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: r.get(0)?,
        user_sub: r.get(1)?,
        email: r.get(2)?,
        created_at: r.get(3)?,
    })
}

// ---------- per-profile settings ----------

pub async fn get_setting(state: &AppState, profile_id: i64, key: &str) -> Result<Option<String>, AppError> {
    let key_s = key.to_string();
    let v = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT value FROM profile_settings WHERE profile_id = ?1 AND key = ?2",
                rusqlite::params![profile_id, key_s],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await?;
    Ok(v)
}

pub async fn set_setting(state: &AppState, profile_id: i64, key: &str, value: &str) -> Result<(), AppError> {
    let key_s = key.to_string();
    let value_s = value.to_string();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO profile_settings (profile_id, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT(profile_id, key) DO UPDATE SET value = excluded.value",
                rusqlite::params![profile_id, key_s, value_s],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

pub async fn delete_setting(state: &AppState, profile_id: i64, key: &str) -> Result<(), AppError> {
    let key_s = key.to_string();
    state
        .db
        .with(move |c| {
            c.execute(
                "DELETE FROM profile_settings WHERE profile_id = ?1 AND key = ?2",
                rusqlite::params![profile_id, key_s],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Resolve a setting with env-fallback semantics:
///   - if profile_settings has a row, return its value
///   - else return the supplied env default
pub async fn effective(
    state: &AppState,
    profile_id: i64,
    key: &str,
    env_default: String,
) -> Result<String, AppError> {
    Ok(get_setting(state, profile_id, key).await?.unwrap_or(env_default))
}

pub fn parse_bool(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

pub async fn all_settings(state: &AppState, profile_id: i64) -> Result<Vec<(String, String)>, AppError> {
    let rows = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare("SELECT key, value FROM profile_settings WHERE profile_id = ?1")?;
            let v = stmt
                .query_map([profile_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    Ok(rows)
}
