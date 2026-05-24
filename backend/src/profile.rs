//! Per-user profile model.
//!
//! v2 introduced `profile`/`profile_settings` tables. Each authenticated
//! user (OIDC sub or DEV_AUTH cookie) maps to exactly one profile row.
//! Audible accounts hang off `accounts.profile_id`, library books +
//! jobs are scoped via that FK.
//!
//! Lookup chain on first contact:
//!   1. `user_sub` match → existing profile
//!   2. `email` match with `user_sub IS NULL` → IaC-seeded row, link sub
//!   3. unknown sub + email → auto-create unless
//!      `SCRIBE_OPEN_REGISTRATION=0` is set
//!
//! `SCRIBE_ADMIN_EMAIL` (when set) pins which email becomes admin on
//! auto-create / first contact. Everyone else gets role=user.

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
    pub role: String,
    pub display_name: Option<String>,
    pub created_at: i64,
}

impl Profile {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// Resolve (or create) the profile for an authenticated request.
///
/// `email` may be `None` for DEV_AUTH when only `?username=` is given —
/// callers should default it to `{sub}@local` before this is reached.
pub async fn resolve_or_create(
    state: &AppState,
    sub: &str,
    email: &str,
    display_name: Option<&str>,
) -> Result<Profile, AppError> {
    let sub_s = sub.to_string();
    let email_s = email.to_lowercase();
    let dn = display_name.map(|s| s.to_string());

    // Fast path: sub already linked.
    if let Some(p) = lookup_by_sub(state, &sub_s).await? {
        return Ok(p);
    }

    // IaC-seeded path: row with matching email but no sub yet — claim it.
    let email_for_link = email_s.clone();
    let sub_for_link = sub_s.clone();
    let linked: Option<Profile> = state
        .db
        .with(move |c| {
            let updated = c.execute(
                "UPDATE profile SET user_sub = ?1 WHERE email = ?2 AND user_sub IS NULL",
                rusqlite::params![sub_for_link, email_for_link],
            )?;
            if updated == 0 {
                return Ok(None);
            }
            let p = c
                .query_row(
                    "SELECT id, user_sub, email, role, display_name, created_at
                     FROM profile WHERE user_sub = ?1",
                    rusqlite::params![sub_for_link],
                    row_to_profile,
                )
                .optional()?;
            Ok(p)
        })
        .await?;
    if let Some(p) = linked {
        return Ok(p);
    }

    // Auto-create policy:
    //   - open_registration=1 → any kanidm-authenticated user gets a profile
    //   - closed + email matches SCRIBE_ADMIN_EMAIL → admin still bootstraps
    //   - closed + no admin match → 403 (random users blocked)
    let is_admin_email = state
        .cfg
        .admin_email
        .as_ref()
        .is_some_and(|e| e.eq_ignore_ascii_case(&email_s));
    if !state.cfg.open_registration && !is_admin_email {
        tracing::warn!(
            sub = %sub_s,
            email = %email_s,
            "closed registration blocked login (email != SCRIBE_ADMIN_EMAIL)"
        );
        return Err(AppError::Forbidden);
    }
    let role = pick_role(state, &email_s).await?;
    create(state, &sub_s, &email_s, dn.as_deref(), &role).await
}

async fn lookup_by_sub(state: &AppState, sub: &str) -> Result<Option<Profile>, AppError> {
    let sub_s = sub.to_string();
    let row = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT id, user_sub, email, role, display_name, created_at
                 FROM profile WHERE user_sub = ?1",
                rusqlite::params![sub_s],
                row_to_profile,
            )
            .optional()
        })
        .await?;
    Ok(row)
}

async fn create(
    state: &AppState,
    sub: &str,
    email: &str,
    display_name: Option<&str>,
    role: &str,
) -> Result<Profile, AppError> {
    let sub_s = sub.to_string();
    let email_s = email.to_string();
    let dn = display_name.map(|s| s.to_string());
    let role_s = role.to_string();
    let now = Utc::now().timestamp();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO profile (user_sub, email, role, display_name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![sub_s, email_s, role_s, dn, now],
            )?;
            Ok(())
        })
        .await?;
    Ok(lookup_by_sub(state, sub).await?.expect("just inserted"))
}

/// Admin role applies to:
///   - the email pinned by `SCRIBE_ADMIN_EMAIL`, OR
///   - the very first profile created (bootstrap-of-last-resort)
async fn pick_role(state: &AppState, email: &str) -> Result<String, AppError> {
    if let Some(admin_email) = &state.cfg.admin_email {
        if admin_email.eq_ignore_ascii_case(email) {
            return Ok("admin".into());
        }
    }
    let count: i64 = state
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM profile", [], |r| r.get(0)))
        .await?;
    Ok(if count == 0 { "admin" } else { "user" }.into())
}

fn row_to_profile(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
    Ok(Profile {
        id: r.get(0)?,
        user_sub: r.get(1)?,
        email: r.get(2)?,
        role: r.get(3)?,
        display_name: r.get(4)?,
        created_at: r.get(5)?,
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
