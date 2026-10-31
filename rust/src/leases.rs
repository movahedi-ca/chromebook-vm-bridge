//! Lease management (audit SEC-7).
//!
//! Fixes vs. the legacy implementation:
//! - `GET /lease/status` NEVER returns the `lease_id` (was hijackable)
//! - `acquire` returns the CLAMPED ttl (was returning unclamped request)
//! - `POST /lease/renew` exists
//! - TTL clamped to [10, 3600]
//! - Browser #1 mutations ALWAYS take the state lock, even with no lease
//!   (leases are coordination, not the only locking — see state.rs)

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

const TTL_MIN: i64 = 10;
const TTL_MAX: i64 = 3600;

pub struct Lease {
    #[allow(dead_code)] // returned on acquire; retained for audit trails
    pub id: String,
    pub client: String,
    pub expires_at: DateTime<Utc>,
}

pub struct LeaseTable {
    leases: HashMap<String, Lease>,
}

impl LeaseTable {
    pub fn new() -> Self {
        Self {
            leases: HashMap::new(),
        }
    }

    /// Acquire a lease. Returns (`lease_id`, `clamped_ttl_s`).
    /// Only one active lease at a time; second acquire gets 423.
    pub fn acquire(
        &mut self,
        client: String,
        requested_ttl_s: i64,
    ) -> Result<(String, i64), LeaseError> {
        self.prune_expired();
        if let Some(existing) = self.leases.values().next() {
            return Err(LeaseError::Locked {
                holder: existing.client.clone(),
                ttl_left_s: (existing.expires_at - Utc::now()).num_seconds().max(0),
            });
        }
        let ttl = requested_ttl_s.clamp(TTL_MIN, TTL_MAX);
        let id = Uuid::new_v4().to_string();
        self.leases.insert(
            id.clone(),
            Lease {
                id: id.clone(),
                client,
                expires_at: Utc::now() + chrono::Duration::seconds(ttl),
            },
        );
        Ok((id, ttl))
    }

    pub fn renew(&mut self, id: &str, requested_ttl_s: i64) -> Result<i64, LeaseError> {
        self.prune_expired();
        let lease = self.leases.get_mut(id).ok_or(LeaseError::NotFound)?;
        let ttl = requested_ttl_s.clamp(TTL_MIN, TTL_MAX);
        lease.expires_at = Utc::now() + chrono::Duration::seconds(ttl);
        Ok(ttl)
    }

    pub fn release(&mut self, id: &str) -> bool {
        self.leases.remove(id).is_some()
    }

    /// Check a presented lease id. Returns true if valid and unexpired.
    pub fn check(&mut self, id: &str) -> bool {
        self.prune_expired();
        self.leases.contains_key(id)
    }

    /// True if any unexpired lease is currently held.
    pub fn locked(&mut self) -> bool {
        self.prune_expired();
        !self.leases.is_empty()
    }

    /// Public status projection for GET /lease/status: the holder's client
    /// name is deliberately NOT included (audit 41 MED-8). The name is
    /// untrusted self-chosen input and a user-activity oracle; only the
    /// bridge itself sees it (server-side logs).
    pub fn status_public(&mut self) -> LeaseStatusPublic {
        self.prune_expired();
        match self.leases.values().next() {
            Some(l) => LeaseStatusPublic {
                locked: true,
                ttl_left_s: (l.expires_at - Utc::now()).num_seconds().max(0),
            },
            None => LeaseStatusPublic {
                locked: false,
                ttl_left_s: 0,
            },
        }
    }

    fn prune_expired(&mut self) {
        let now = Utc::now();
        self.leases.retain(|_, l| l.expires_at > now);
    }
}

impl Default for LeaseTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Public view of a lease for GET /lease/status: the holder's client name
/// is deliberately NOT included. The name is untrusted self-chosen input
/// and a user-activity oracle ("is the operator active right now?"); the
/// module's design note has always said it must not be surfaced, but the
/// route used to serialize the full struct (audit 41 MED-8).
#[derive(Debug, Serialize)]
pub struct LeaseStatusPublic {
    pub locked: bool,
    #[serde(rename = "ttl_left_s")]
    pub ttl_left_s: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    // The holder's self-chosen client name is NOT surfaced to callers
    // (audit LOW-2): it is untrusted input and a reflection surface.
    // Only the TTL is public; the holder is logged server-side.
    #[error("lease held ({ttl_left_s}s left)")]
    Locked { holder: String, ttl_left_s: i64 },
    #[error("lease not found or expired")]
    NotFound,
}
