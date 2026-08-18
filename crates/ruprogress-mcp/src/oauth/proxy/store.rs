//! `oauth-proxy` DCR client registry (P8, C7, C8): a bounded, `Mutex`-guarded
//! map, not a database — the whole store is gone on restart (P4), and every
//! operation is short enough that holding the lock across it (never across
//! an `.await`) is the right call rather than an async lock.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use rand::TryRngCore as _;
use rand::rngs::OsRng;

/// A registered DCR client (P8): every client is public — `/register` never
/// issues, and `/token` never accepts, a `client_secret`.
#[derive(Debug, Clone)]
pub(crate) struct ClientRegistration {
    pub(crate) client_id: String,
    pub(crate) redirect_uris: Vec<String>,
    pub(crate) client_name: Option<String>,
}

struct Entry {
    registration: ClientRegistration,
    /// Touched at registration, and (once `/authorize` and `/token` exist)
    /// on every use — the basis [`ClientRegistry::register`]'s eviction
    /// picks the least-recently-used *idle* entry from.
    last_seen: Instant,
    /// Whether this client holds a live token. Always `false` today (no
    /// token can exist without `/token`); the field exists now so eviction
    /// already prefers idle registrations once tokens do.
    live: bool,
}

/// Hard cap on registrations (P8, C8): an unauthenticated endpoint that
/// allocates on every call must not be an unbounded allocator.
const MAX_CLIENTS: usize = 1000;

/// Bounded DCR client registry. `Debug` prints counts, never a registration's
/// contents (a redirect URI list is operator-configured-adjacent data, not a
/// secret, but there is no reason to make it walkable from a core dump
/// either).
pub(crate) struct ClientRegistry {
    inner: Mutex<HashMap<String, Entry>>,
}

impl std::fmt::Debug for ClientRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("ClientRegistry")
            .field("len", &len)
            .field("capacity", &MAX_CLIENTS)
            .finish()
    }
}

impl ClientRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 128 bits of `OsRng`, hex-encoded (C7): public by specification, so a
    /// plain map key rather than a digest. `OsRng` in `rand` 0.9 is
    /// fallible (`TryRngCore`); a failure is exceedingly rare (a broken
    /// host, not caller input) and is reported as `None` rather than a
    /// panic — the caller treats it the same as a full store.
    fn mint_client_id() -> Option<String> {
        let mut bytes = [0u8; 16];
        if let Err(error) = OsRng.try_fill_bytes(&mut bytes) {
            tracing::error!(%error, "OS RNG unavailable; cannot mint a DCR client_id");
            return None;
        }
        let mut id = String::with_capacity(32);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(id, "{byte:02x}");
        }
        Some(id)
    }

    /// Registers a new client. `None` means the store is full of live
    /// registrations, or `OsRng` failed (C8) — the caller turns either into
    /// a `503` with `Retry-After`.
    pub(crate) fn register(
        &self,
        redirect_uris: Vec<String>,
        client_name: Option<String>,
    ) -> Option<ClientRegistration> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.len() >= MAX_CLIENTS {
            Self::evict_idle(&mut inner);
        }
        if inner.len() >= MAX_CLIENTS {
            return None;
        }
        let client_id = Self::mint_client_id()?;
        let registration = ClientRegistration {
            client_id: client_id.clone(),
            redirect_uris,
            client_name,
        };
        inner.insert(
            client_id,
            Entry {
                registration: registration.clone(),
                last_seen: Instant::now(),
                live: false,
            },
        );
        Some(registration)
    }

    /// Evicts the least-recently-used entry with no live token, if any.
    /// Degrades gracefully: if every entry is live, this is a no-op and
    /// [`Self::register`] reports the store full.
    fn evict_idle(inner: &mut HashMap<String, Entry>) {
        let victim = inner
            .iter()
            .filter(|(_, entry)| !entry.live)
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(client_id, _)| client_id.clone());
        if let Some(client_id) = victim {
            inner.remove(&client_id);
        }
    }

    #[allow(
        dead_code,
        reason = "consumed by /authorize and /token once they exist, which look up a registered \
                  client by id; exercised here by this module's own tests in the meantime"
    )]
    pub(crate) fn get(&self, client_id: &str) -> Option<ClientRegistration> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .map(|entry| entry.registration.clone())
    }

    #[cfg(test)]
    fn set_live(&self, client_id: &str, live: bool) {
        if let Some(entry) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(client_id)
        {
            entry.live = live;
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn register_returns_a_32_char_hex_client_id() {
        let registry = ClientRegistry::new();
        let registration = registry
            .register(vec!["http://localhost/cb".to_string()], None)
            .expect("should register");
        assert_eq!(registration.client_id.len(), 32);
        assert!(
            registration
                .client_id
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
    }

    #[test]
    fn registered_client_is_retrievable_by_id() {
        let registry = ClientRegistry::new();
        let registration = registry
            .register(
                vec!["http://localhost/cb".to_string()],
                Some("cli".to_string()),
            )
            .expect("should register");
        let fetched = registry.get(&registration.client_id).expect("should exist");
        assert_eq!(fetched.redirect_uris, vec!["http://localhost/cb"]);
        assert_eq!(fetched.client_name.as_deref(), Some("cli"));
    }

    #[test]
    fn unknown_client_id_is_absent() {
        let registry = ClientRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn two_registrations_never_collide() {
        let registry = ClientRegistry::new();
        let a = registry.register(vec![], None).expect("should register");
        let b = registry.register(vec![], None).expect("should register");
        assert_ne!(a.client_id, b.client_id);
    }

    #[test]
    fn debug_prints_counts_not_registration_contents() {
        let registry = ClientRegistry::new();
        registry
            .register(
                vec!["https://secret-looking-host.example/cb".to_string()],
                None,
            )
            .expect("should register");
        let rendered = format!("{registry:?}");
        assert!(!rendered.contains("secret-looking-host"));
        assert!(rendered.contains("len"));
    }

    #[test]
    fn overflow_evicts_the_oldest_idle_registration() {
        let registry = ClientRegistry::new();
        let first = registry.register(vec![], None).expect("should register");
        for _ in 1..MAX_CLIENTS {
            registry.register(vec![], None).expect("should register");
        }
        assert_eq!(registry.len(), MAX_CLIENTS);

        // One more push should evict `first` (the oldest) rather than fail.
        let newest = registry
            .register(vec![], None)
            .expect("eviction should make room");
        assert_eq!(registry.len(), MAX_CLIENTS);
        assert!(registry.get(&first.client_id).is_none());
        assert!(registry.get(&newest.client_id).is_some());
    }

    #[test]
    fn overflow_prefers_evicting_an_idle_registration_over_a_live_one() {
        let registry = ClientRegistry::new();
        let live = registry.register(vec![], None).expect("should register");
        registry.set_live(&live.client_id, true);
        for _ in 1..MAX_CLIENTS {
            registry.register(vec![], None).expect("should register");
        }
        assert_eq!(registry.len(), MAX_CLIENTS);

        registry
            .register(vec![], None)
            .expect("an idle slot exists");
        // The live registration must survive even though it was the oldest.
        assert!(registry.get(&live.client_id).is_some());
    }

    #[test]
    fn registration_is_refused_once_every_slot_is_live() {
        let registry = ClientRegistry::new();
        for _ in 0..MAX_CLIENTS {
            let registration = registry.register(vec![], None).expect("should register");
            registry.set_live(&registration.client_id, true);
        }
        assert!(registry.register(vec![], None).is_none());
    }
}
