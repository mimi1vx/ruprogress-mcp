//! `oauth-proxy` state: the DCR client registry (P8, C7, C8), the
//! in-flight-transaction/authorization-code/token stores (F2, F6, F9), all
//! bounded, `Mutex`-guarded maps rather than a database — the whole store is
//! gone on restart (P4), and every operation is short enough that holding
//! the lock across it (never across an `.await`, F12) is the right call
//! rather than an async lock.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore as _;
use rand::rngs::OsRng;
use secrecy::SecretString;
use sha2::{Digest as _, Sha256};

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
    /// Touched at registration and on every validated use (`/authorize`,
    /// an `authorization_code` redemption, a `refresh_token` grant) — the
    /// idle clock [`ClientRegistry::register`]'s capacity sweep measures
    /// against [`CLIENT_IDLE_TTL`].
    last_seen: Instant,
}

/// Hard cap on registrations (P8, C8): an unauthenticated endpoint that
/// allocates on every call must not be an unbounded allocator.
const MAX_CLIENTS: usize = 1000;

/// How long a registration may sit idle (no `/authorize`, code redemption,
/// or refresh use) before it becomes reclaimable under capacity pressure.
/// Comfortably above [`TRANSACTION_TTL`], so a registration
/// mid-authorization is never at risk; caps an attacker-driven `/register`
/// lockout of a given `client_id` at an hour.
const CLIENT_IDLE_TTL: Duration = Duration::from_hours(1);

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

    /// Registers a new client. `live` is the union of every `client_id`
    /// currently holding an unexpired proxy access or refresh token
    /// ([`TokenStore::live_client_ids`], [`RefreshStore::live_client_ids`]),
    /// built by the caller ([`ProxyState::register_client`]) before this
    /// lock is taken. `None` means the store is still full once reclaimable
    /// entries are swept, or `OsRng` failed (C8) — the caller turns either
    /// into a `503` with `Retry-After`.
    pub(crate) fn register(
        &self,
        redirect_uris: Vec<String>,
        client_name: Option<String>,
        live: &HashSet<String>,
    ) -> Option<ClientRegistration> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.len() >= MAX_CLIENTS {
            let now = Instant::now();
            let before = inner.len();
            inner.retain(|client_id, entry| {
                live.contains(client_id)
                    || now.saturating_duration_since(entry.last_seen) < CLIENT_IDLE_TTL
            });
            if inner.len() < before {
                tracing::debug!(
                    len = inner.len(),
                    capacity = MAX_CLIENTS,
                    "swept idle DCR client registrations to make room"
                );
            }
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
            },
        );
        Some(registration)
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

    /// Marks `client_id` as used just now, resetting the idle clock
    /// [`Self::register`]'s capacity sweep measures against. A no-op if the
    /// client is unknown.
    pub(crate) fn touch(&self, client_id: &str) {
        if let Some(entry) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(client_id)
        {
            entry.last_seen = Instant::now();
        }
    }

    /// Moves `client_id`'s `last_seen` backwards by `by`, so a test can age
    /// an entry past [`CLIENT_IDLE_TTL`] without waiting an hour — the same
    /// shape as the `set_live` seam this replaces.
    #[cfg(test)]
    fn rewind(&self, client_id: &str, by: Duration) {
        if let Some(entry) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(client_id)
        {
            entry.last_seen = entry.last_seen.checked_sub(by).unwrap_or(entry.last_seen);
        }
    }

    /// Live registration count, for `get_mcp_server_info`'s
    /// `registered_clients` (R7) — a count only, never a client id or name.
    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// `Instant::now() + ttl`, saturating rather than risking an overflow panic
/// on a duration this store itself always chooses (never caller-supplied
/// arithmetic on untrusted input, but `checked_add` costs nothing and keeps
/// `clippy::arithmetic_side_effects` honest).
pub(crate) fn expires_after(ttl: Duration) -> Instant {
    Instant::now().checked_add(ttl).unwrap_or_else(Instant::now)
}

/// SHA-256 digest, used as the map key for every store below that is keyed
/// on a value a client presents back to us (P2, F2, F9): the plaintext
/// never appears in a `Debug`/core-dump-walkable key.
fn digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

/// 256 bits of `OsRng`, base64url-encoded (no padding), prefixed with
/// `prefix`. Shared mint routine for transaction ids, authorization codes,
/// and access tokens — all opaque CSPRNG handles per P2. `None` only on OS
/// RNG failure (C8's failure mode), never a caller-input failure.
fn mint_opaque_token(prefix: &str) -> Option<String> {
    let mut bytes = [0u8; 32];
    if let Err(error) = OsRng.try_fill_bytes(&mut bytes) {
        tracing::error!(%error, prefix, "OS RNG unavailable; cannot mint an opaque token");
        return None;
    }
    Some(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// Hard cap shared by every bounded map below (P4, C8): generous for one
/// process's in-flight state, small enough that an unauthenticated or
/// lightly-authenticated endpoint cannot become an unbounded allocator.
const MAX_ENTRIES: usize = 10_000;

/// Drops every expired entry from `map`, then refuses the insert (returning
/// `false`) if `map` is still at [`MAX_ENTRIES`] and does not already
/// contain `key` — the same sweep-then-cap shape
/// `auth::oauth::TokenVerifier::cache_put` uses. Never evicts a live entry
/// to make room: a full store degrades by rejecting new state, not by
/// dropping someone else's in-flight flow.
fn sweep_and_check_capacity<K: std::hash::Hash + Eq, V>(
    map: &mut HashMap<K, (V, Instant)>,
    key: &K,
) -> bool {
    let now = Instant::now();
    map.retain(|_, (_, expires_at)| *expires_at > now);
    let has_room = map.len() < MAX_ENTRIES || map.contains_key(key);
    if !has_room {
        tracing::debug!(
            len = map.len(),
            capacity = MAX_ENTRIES,
            "store at capacity; rejecting a new entry"
        );
    }
    has_room
}

// --- in-flight authorization transactions (F2) ------------------------------

/// How long an authorization transaction survives between `/authorize` and
/// `/auth/callback` before it is treated as abandoned.
const TRANSACTION_TTL: Duration = Duration::from_mins(10);

/// One in-flight `/authorize` → `/auth/callback` round trip (F2): every
/// parameter the callback needs to finish the flow, captured at
/// `/authorize` time.
pub(crate) struct Transaction {
    pub(crate) client_id: String,
    pub(crate) redirect_uri: String,
    pub(crate) code_challenge: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) client_state: Option<String>,
    pub(crate) upstream_code_verifier: String,
}

/// Digest-keyed (the raw id is also the upstream `state`, which transits the
/// user's browser — P2's threat model applies here too), single-use,
/// TTL-bounded.
#[derive(Default)]
pub(crate) struct TransactionStore {
    inner: Mutex<HashMap<[u8; 32], (Transaction, Instant)>>,
}

impl std::fmt::Debug for TransactionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("TransactionStore")
            .field("len", &len)
            .finish()
    }
}

impl TransactionStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stores `transaction`, returning the raw id to send upstream as
    /// `state`. `None` means the store is full or `OsRng` failed (C8); the
    /// caller turns either into a `503`.
    pub(crate) fn create(&self, transaction: Transaction) -> Option<String> {
        let id = mint_opaque_token("")?;
        let key = digest(&id);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !sweep_and_check_capacity(&mut inner, &key) {
            return None;
        }
        inner.insert(key, (transaction, expires_after(TRANSACTION_TTL)));
        Some(id)
    }

    /// Consumes the transaction named by `state`, if it exists and has not
    /// expired. Single-use: a second call with the same `state` returns
    /// `None`, closing off transaction replay.
    pub(crate) fn take(&self, state: &str) -> Option<Transaction> {
        let key = digest(state);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (transaction, expires_at) = inner.remove(&key)?;
        (expires_at > Instant::now()).then_some(transaction)
    }
}

// --- the upstream token this server holds on the user's behalf (F6) --------

/// What `/auth/callback`'s upstream exchange produced: never sent to the
/// client (P10), retrieved by `auth::proxy`'s middleware on every request a
/// proxy access token authenticates.
pub(crate) struct UpstreamTokenSet {
    pub(crate) access: SecretString,
    /// Present only when Doorkeeper's `use_refresh_token` is enabled (R4).
    pub(crate) refresh: Option<SecretString>,
    pub(crate) granted_scopes: Vec<String>,
    pub(crate) expires_at: Instant,
}

/// Keyed by a plain (non-digest) internal id: unlike a transaction id or a
/// proxy token, this id is never observed outside this process — not in a
/// URL, not in a response body — so digest-keying would add nothing.
#[derive(Default)]
pub(crate) struct UpstreamStore {
    inner: Mutex<HashMap<String, (UpstreamTokenSet, Instant)>>,
}

impl std::fmt::Debug for UpstreamStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("UpstreamStore").field("len", &len).finish()
    }
}

impl UpstreamStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The deadline past which a session is swept. Deliberately **not**
    /// `set.expires_at` when a refresh token is present: that field is the
    /// *upstream access token's* own expiry, and a session holding a
    /// refresh token stays legitimately usable long after its access token
    /// expires — the client just calls `/token`'s `refresh_token` grant,
    /// and Doorkeeper refresh tokens never expire on their own upstream. So
    /// this store needs its own bound for a refreshable session, and
    /// borrows [`REFRESH_TTL`] (the same 30-day clock the proxy's own
    /// refresh token already uses) rather than inventing a second constant.
    /// A session with no refresh token has nothing to fall back on once its
    /// access token expires, so its deadline is exactly `expires_at`.
    fn session_deadline(set: &UpstreamTokenSet) -> Instant {
        if set.refresh.is_some() {
            expires_after(REFRESH_TTL)
        } else {
            set.expires_at
        }
    }

    /// Stores `set`, returning the internal id it was stored under. `None`
    /// means the store is full of live sessions, or `OsRng` failed (C8).
    pub(crate) fn insert(&self, set: UpstreamTokenSet) -> Option<String> {
        let mut bytes = [0u8; 16];
        if let Err(error) = OsRng.try_fill_bytes(&mut bytes) {
            tracing::error!(%error, "OS RNG unavailable; cannot mint an upstream-token-set id");
            return None;
        }
        let mut id = String::with_capacity(32);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(id, "{byte:02x}");
        }
        let deadline = Self::session_deadline(&set);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !sweep_and_check_capacity(&mut inner, &id) {
            return None;
        }
        inner.insert(id.clone(), (set, deadline));
        Some(id)
    }

    /// A clone of the stored access token, for the middleware to hand to
    /// [`crate::auth::oauth::TokenVerifier::verify`] on every request. Fails
    /// closed past the session's deadline without removing it — removal
    /// must go through [`super::store::ProxyState::take_session`] or
    /// [`super::store::ProxyState::sweep_expired_sessions`] so the
    /// dependent proxy tokens are purged too.
    pub(crate) fn access_token(&self, id: &str) -> Option<SecretString> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, deadline) = inner.get(id)?;
        (*deadline > Instant::now()).then(|| set.access.clone())
    }

    /// A clone of the stored refresh token, if Doorkeeper issued one (R4),
    /// for the `/token` refresh grant to present upstream. Fails closed
    /// past the session's deadline, same reasoning as [`Self::access_token`].
    pub(crate) fn refresh_token(&self, id: &str) -> Option<SecretString> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, deadline) = inner.get(id)?;
        if *deadline <= Instant::now() {
            return None;
        }
        set.refresh.clone()
    }

    /// A clone of the stored granted scopes, for the `/token` refresh grant
    /// to fall back to when Doorkeeper's refresh response omits `scope`
    /// (RFC 6749 §6: absent means unchanged). Fails closed past the
    /// session's deadline, same reasoning as [`Self::access_token`].
    pub(crate) fn granted_scopes(&self, id: &str) -> Option<Vec<String>> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (set, deadline) = inner.get(id)?;
        (*deadline > Instant::now()).then(|| set.granted_scopes.clone())
    }

    /// Removes and returns the stored set regardless of its deadline, for a
    /// caller that needs the refresh token of an access-expired session to
    /// revoke it upstream (R5's `/revoke`, R2's reuse containment) — unlike
    /// [`Self::access_token`] et al., a past-deadline entry is still
    /// present here, not absent. Does not purge the dependent proxy tokens;
    /// callers reach this through
    /// [`super::store::ProxyState::take_session`], which does.
    pub(crate) fn take(&self, id: &str) -> Option<UpstreamTokenSet> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .map(|(set, _)| set)
    }

    /// Replaces the stored set in place and renews its deadline, keeping
    /// `id` stable across a refresh (R1): the proxy access/refresh tokens
    /// minted before the refresh, and any bookkeeping keyed on `id`, all
    /// still resolve to the same session afterward. Conditional on `id`
    /// still being present *and not past its deadline* (finding 2): an
    /// in-flight refresh must not resurrect a session a concurrent
    /// `/revoke`, reuse-containment path, or sweep already removed. On
    /// `Err`, `set` is handed back unused so the caller can revoke the
    /// upstream secret it just obtained instead of it being silently
    /// dropped.
    #[must_use = "Err(set) hands back an unused upstream token set that must be revoked"]
    pub(crate) fn replace(&self, id: &str, set: UpstreamTokenSet) -> Result<(), UpstreamTokenSet> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        match inner.get_mut(id) {
            Some((existing, deadline)) if *deadline > Instant::now() => {
                *deadline = Self::session_deadline(&set);
                *existing = set;
                Ok(())
            }
            _ => Err(set),
        }
    }

    /// Live (not past its deadline) upstream-session count, for
    /// `get_mcp_server_info`'s `active_sessions` (R7).
    pub(crate) fn len(&self) -> usize {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|(_, deadline)| *deadline > now)
            .count()
    }

    /// Removes every session past its deadline, returning what each held.
    /// Does not purge the dependent proxy tokens or revoke anything
    /// upstream — callers reach this through
    /// [`super::store::ProxyState::sweep_expired_sessions`], which does
    /// both, since neither is reachable from this module.
    #[must_use]
    pub(crate) fn sweep_expired(&self) -> Vec<(String, UpstreamTokenSet)> {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner
            .extract_if(|_, (_, deadline)| *deadline <= now)
            .map(|(id, (set, _))| (id, set))
            .collect()
    }
}

// --- authorization codes (F6, F7, F8) ---------------------------------------

/// How long a minted authorization code is redeemable.
const CODE_TTL: Duration = Duration::from_mins(1);

struct PendingCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
}

/// What a successfully redeemed code minted, kept for [`CODE_TTL`] after
/// redemption so a replay of the same code can be detected and contained
/// (F8) rather than just bouncing off "not found".
struct ConsumedRecord {
    minted_token_digest: [u8; 32],
    minted_refresh_digest: Option<[u8; 32]>,
    upstream_id: String,
}

/// Outcome of [`CodeStore::redeem`].
pub(crate) enum RedeemOutcome {
    /// The code does not exist, already expired, or was never valid —
    /// indistinguishable from a checks-mismatch by design (F7): both are
    /// `invalid_grant` with no further detail.
    Invalid,
    /// `client_id`, `redirect_uri`, or the PKCE verifier did not match this
    /// code's bindings. The code is left untouched (F7): a legitimate
    /// client's later, correct retry within the TTL must still work.
    Mismatch,
    /// The code had already been redeemed once; contains what that first
    /// redemption minted so the caller can revoke it (F8).
    Replayed {
        minted_token_digest: [u8; 32],
        minted_refresh_digest: Option<[u8; 32]>,
        upstream_id: String,
    },
    /// First, successful redemption: the code is now consumed. The caller
    /// must call [`CodeStore::mark_consumed`] once it knows what it minted,
    /// so a subsequent replay within [`CODE_TTL`] is caught.
    Ok(UpstreamTokenSet),
}

#[derive(Default)]
pub(crate) struct CodeStore {
    pending: Mutex<HashMap<[u8; 32], (PendingCode, Instant)>>,
    upstream: Mutex<HashMap<[u8; 32], UpstreamTokenSet>>,
    consumed: Mutex<HashMap<[u8; 32], (ConsumedRecord, Instant)>>,
}

impl std::fmt::Debug for CodeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let upstream = self
            .upstream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let consumed = self
            .consumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("CodeStore")
            .field("pending", &pending)
            .field("upstream", &upstream)
            .field("consumed", &consumed)
            .finish()
    }
}

impl CodeStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mints a `rup_ac_`-prefixed authorization code bound to `client_id` +
    /// `redirect_uri` + `code_challenge` + `upstream` (F6). `None` means the
    /// store is full or `OsRng` failed (C8).
    pub(crate) fn mint(
        &self,
        client_id: String,
        redirect_uri: String,
        code_challenge: String,
        upstream: UpstreamTokenSet,
    ) -> Option<String> {
        let code = mint_opaque_token("rup_ac_")?;
        let key = digest(&code);
        let pending_entry = PendingCode {
            client_id,
            redirect_uri,
            code_challenge,
        };
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if !sweep_and_check_capacity(&mut pending, &key) {
            return None;
        }
        self.upstream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, upstream);
        pending.insert(key, (pending_entry, expires_after(CODE_TTL)));
        Some(code)
    }

    /// F7's ordered checks, then F8's replay containment. Never consumes the
    /// code on anything but a first, fully-matching redemption.
    pub(crate) fn redeem(
        &self,
        code: &str,
        client_id: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> RedeemOutcome {
        let key = digest(code);

        let pending_entry = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            match pending.get(&key) {
                Some((_, expires_at)) if *expires_at <= Instant::now() => {
                    pending.remove(&key);
                    None
                }
                Some((entry, _)) => Some(PendingCode {
                    client_id: entry.client_id.clone(),
                    redirect_uri: entry.redirect_uri.clone(),
                    code_challenge: entry.code_challenge.clone(),
                }),
                None => None,
            }
        };

        let Some(entry) = pending_entry else {
            let consumed = self.consumed.lock().unwrap_or_else(PoisonError::into_inner);
            return match consumed.get(&key) {
                Some((record, expires_at)) if *expires_at > Instant::now() => {
                    RedeemOutcome::Replayed {
                        minted_token_digest: record.minted_token_digest,
                        minted_refresh_digest: record.minted_refresh_digest,
                        upstream_id: record.upstream_id.clone(),
                    }
                }
                _ => RedeemOutcome::Invalid,
            };
        };

        if entry.client_id != client_id
            || entry.redirect_uri != redirect_uri
            || !super::pkce::verify(&entry.code_challenge, code_verifier)
        {
            return RedeemOutcome::Mismatch;
        }

        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        let Some(upstream) = self
            .upstream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key)
        else {
            // The upstream half is always inserted alongside the pending
            // entry in `mint`; its absence here would mean this code was
            // never actually minted by this store.
            return RedeemOutcome::Invalid;
        };
        RedeemOutcome::Ok(upstream)
    }

    /// Records what a successful [`Self::redeem`] minted, so a replay of
    /// the same code within [`CODE_TTL`] is caught (F8) instead of falling
    /// through to "not found".
    pub(crate) fn mark_consumed(
        &self,
        code: &str,
        minted_token_digest: [u8; 32],
        minted_refresh_digest: Option<[u8; 32]>,
        upstream_id: String,
    ) {
        let key = digest(code);
        let mut consumed = self.consumed.lock().unwrap_or_else(PoisonError::into_inner);
        if sweep_and_check_capacity(&mut consumed, &key) {
            consumed.insert(
                key,
                (
                    ConsumedRecord {
                        minted_token_digest,
                        minted_refresh_digest,
                        upstream_id,
                    },
                    expires_after(CODE_TTL),
                ),
            );
        }
    }
}

// --- proxy access tokens (F9) ------------------------------------------------

pub(crate) struct TokenEntry {
    pub(crate) upstream_id: String,
    pub(crate) client_id: String,
}

/// Digest-keyed (P2): the plaintext `rup_at_...` token never appears as a
/// map key walkable from a core dump.
#[derive(Default)]
pub(crate) struct TokenStore {
    inner: Mutex<HashMap<[u8; 32], (TokenEntry, Instant)>>,
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("TokenStore").field("len", &len).finish()
    }
}

impl TokenStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mints a `rup_at_`-prefixed proxy access token bound to `upstream_id`,
    /// expiring after `ttl` (P10: never longer than the upstream token's own
    /// remaining lifetime). Returns the raw token and its digest — the
    /// caller needs the digest to hand to [`CodeStore::mark_consumed`] for
    /// F8's replay containment. `None` means the store is full or `OsRng`
    /// failed (C8).
    pub(crate) fn mint(
        &self,
        client_id: String,
        upstream_id: String,
        ttl: Duration,
    ) -> Option<(String, [u8; 32])> {
        let token = mint_opaque_token("rup_at_")?;
        let key = digest(&token);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !sweep_and_check_capacity(&mut inner, &key) {
            return None;
        }
        inner.insert(
            key,
            (
                TokenEntry {
                    upstream_id,
                    client_id,
                },
                expires_after(ttl),
            ),
        );
        Some((token, key))
    }

    /// Resolves a presented `rup_at_...` token to its `upstream_id`, or
    /// `None` if it is unknown or expired (F10 folds both into
    /// `invalid_token`).
    pub(crate) fn resolve(&self, token: &str) -> Option<TokenEntry> {
        let key = digest(token);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (entry, expires_at) = inner.get(&key)?;
        if *expires_at <= Instant::now() {
            inner.remove(&key);
            return None;
        }
        Some(TokenEntry {
            upstream_id: entry.upstream_id.clone(),
            client_id: entry.client_id.clone(),
        })
    }

    /// Deletes a proxy access token by its digest (F8's replay containment;
    /// R5's `/revoke`).
    pub(crate) fn delete_by_digest(&self, digest: [u8; 32]) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&digest);
    }

    /// Removes and returns the entry for a presented `rup_at_...` token,
    /// regardless of expiry (R5's `/revoke`): unlike [`Self::resolve`], an
    /// already-expired entry is still removed and returned rather than
    /// treated as absent.
    pub(crate) fn take(&self, token: &str) -> Option<TokenEntry> {
        let key = digest(token);
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key)
            .map(|(entry, _)| entry)
    }

    /// Removes every proxy access token bound to `upstream_id` (session
    /// removal's referential cleanup, finding 6): a `retain` scan over a
    /// map capped at [`MAX_ENTRIES`], run only from
    /// `super::store::ProxyState::take_session`/`sweep_expired_sessions`,
    /// never the resolve path.
    pub(crate) fn remove_by_upstream(&self, upstream_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, (entry, _)| entry.upstream_id != upstream_id);
    }

    /// Every `client_id` with a currently unexpired proxy access token:
    /// half of the anti-eviction live set
    /// [`ProxyState::register_client`] passes to [`ClientRegistry::register`].
    /// Read-only — an expired entry is left in place for its own resolve
    /// path or sweep to remove, not evicted from here.
    pub(crate) fn live_client_ids(&self) -> HashSet<String> {
        let now = Instant::now();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(entry, _)| entry.client_id.clone())
            .collect()
    }
}

// --- proxy refresh tokens (R1, R2) ---------------------------------------

/// A memory-bound safety net (P4), not a real credential lifetime: upstream
/// is authoritative for whether a refresh token is still honoured, so this
/// only bounds how long an *abandoned* one occupies the store.
const REFRESH_TTL: Duration = Duration::from_hours(720);

/// How long a retired (already-rotated) refresh-token digest is remembered
/// so a later replay of it is caught as reuse (R2) rather than merely
/// unrecognised.
const RETIRED_REFRESH_TTL: Duration = Duration::from_hours(24);

/// Whether an active refresh entry is available to redeem, or is currently
/// being redeemed by another (or the same, racing) request.
enum RefreshState {
    Active,
    InFlight,
}

struct RefreshOwner {
    client_id: String,
    upstream_id: String,
    state: RefreshState,
}

/// Outcome of [`RefreshStore::redeem`].
pub(crate) enum RefreshOutcome<'a> {
    /// Unknown, expired, or never issued — indistinguishable by design,
    /// same reasoning as [`CodeStore`]'s `Invalid` (F7).
    Invalid,
    /// This digest was already rotated away (a replay, R2), *or* it is
    /// still active but another request is already redeeming it — a
    /// concurrent double-use is indistinguishable from an attacker racing
    /// the legitimate client, so both are treated as reuse. `upstream_id`
    /// is the session's stable identifier across every rotation in its
    /// chain (see [`UpstreamStore::replace`]), so the caller can revoke
    /// whatever is *currently* live for it, however many rotations ago
    /// this particular token was current.
    Reused { upstream_id: String },
    /// A live, not-yet-rotated refresh token bound to `client_id` and
    /// `upstream_id`, now marked `InFlight`. `guard` restores it to
    /// `Active` on drop unless the caller commits via `retire`, `discard`,
    /// or `take` first — all three remove the entry, making the guard a
    /// no-op on every committed path.
    Ok {
        client_id: String,
        upstream_id: String,
        guard: InFlightGuard<'a>,
    },
}

/// Restores an `InFlight` refresh entry to `Active` when dropped without a
/// prior commit — covers a dropped handler future (client disconnect), an
/// early return, or an unwinding panic (`panic = "unwind"` is pinned in the
/// release profile so this runs). Deliberately has no `disarm`/`commit`
/// method: `retire`, `discard`, and `take` all remove the entry outright, so
/// the guard's rollback is a no-op on every path that actually finishes the
/// refresh, and rollback-by-default is the safe behaviour for every path
/// that doesn't.
pub(crate) struct InFlightGuard<'a> {
    store: &'a RefreshStore,
    key: [u8; 32],
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        let mut current = self
            .store
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((owner, _)) = current.get_mut(&self.key)
            && matches!(owner.state, RefreshState::InFlight)
        {
            owner.state = RefreshState::Active;
        }
    }
}

/// Digest-keyed (P2), two maps: `current` is the one active refresh token
/// per session, `retired` is what [`Self::retire`] moves a rotated-away
/// digest into so [`Self::redeem`] can tell a replay apart from "unknown".
#[derive(Default)]
pub(crate) struct RefreshStore {
    current: Mutex<HashMap<[u8; 32], (RefreshOwner, Instant)>>,
    retired: Mutex<HashMap<[u8; 32], (String, Instant)>>,
}

impl std::fmt::Debug for RefreshStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let retired = self
            .retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("RefreshStore")
            .field("current", &current)
            .field("retired", &retired)
            .finish()
    }
}

impl RefreshStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mints a `rup_rt_`-prefixed proxy refresh token bound to `client_id` +
    /// `upstream_id`. Returns the raw token and its digest — the caller
    /// needs the digest to hand to [`CodeStore::mark_consumed`] for F8's
    /// replay containment. `None` means the store is full or `OsRng`
    /// failed (C8).
    pub(crate) fn mint(
        &self,
        client_id: String,
        upstream_id: String,
    ) -> Option<(String, [u8; 32])> {
        let token = mint_opaque_token("rup_rt_")?;
        let key = digest(&token);
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if !sweep_and_check_capacity(&mut current, &key) {
            return None;
        }
        current.insert(
            key,
            (
                RefreshOwner {
                    client_id,
                    upstream_id,
                    state: RefreshState::Active,
                },
                expires_after(REFRESH_TTL),
            ),
        );
        Some((token, key))
    }

    /// Atomically transitions `token` from `Active` to `InFlight` under one
    /// lock acquisition, so two concurrent redemptions of the same token
    /// can never both succeed (finding 2): the second sees `InFlight` and
    /// is treated as reuse. The caller mints the new pair and confirms it
    /// is durable *before* calling [`Self::retire`] (risk 1: the new pair
    /// must work before the old one stops) — until then, the returned
    /// guard keeps the token locked out.
    pub(crate) fn redeem(&self, token: &str) -> RefreshOutcome<'_> {
        let key = digest(token);
        {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            match current.get_mut(&key) {
                Some((_, expires_at)) if *expires_at <= Instant::now() => {
                    current.remove(&key);
                }
                Some((owner, _)) => {
                    return match owner.state {
                        RefreshState::InFlight => RefreshOutcome::Reused {
                            upstream_id: owner.upstream_id.clone(),
                        },
                        RefreshState::Active => {
                            owner.state = RefreshState::InFlight;
                            RefreshOutcome::Ok {
                                client_id: owner.client_id.clone(),
                                upstream_id: owner.upstream_id.clone(),
                                guard: InFlightGuard { store: self, key },
                            }
                        }
                    };
                }
                None => {}
            }
        }
        let retired = self.retired.lock().unwrap_or_else(PoisonError::into_inner);
        match retired.get(&key) {
            Some((upstream_id, expires_at)) if *expires_at > Instant::now() => {
                RefreshOutcome::Reused {
                    upstream_id: upstream_id.clone(),
                }
            }
            _ => RefreshOutcome::Invalid,
        }
    }

    /// Rotation (R1): `old_token` is removed from the active set and
    /// remembered in the retired set for [`RETIRED_REFRESH_TTL`], so a
    /// later replay of it is caught rather than bouncing off "not found".
    pub(crate) fn retire(&self, old_token: &str, upstream_id: String) {
        let key = digest(old_token);
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        let mut retired = self.retired.lock().unwrap_or_else(PoisonError::into_inner);
        if sweep_and_check_capacity(&mut retired, &key) {
            retired.insert(key, (upstream_id, expires_after(RETIRED_REFRESH_TTL)));
        }
    }

    /// Removes `old_token` from the active set with no reuse tracking: for
    /// a refresh that upstream itself rejected, where there is no
    /// legitimate rotation to protect against replay of.
    pub(crate) fn discard(&self, old_token: &str) {
        let key = digest(old_token);
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }

    /// Removes and returns the owner of a presented `rup_rt_...` token, for
    /// `/revoke` (R5). A retired or unknown token resolves to `None` —
    /// revoking either is a no-op per RFC 7009, not a reuse signal (that
    /// containment is `/token`'s alone, see [`Self::redeem`]).
    pub(crate) fn take(&self, token: &str) -> Option<(String, String)> {
        let key = digest(token);
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key)
            .map(|(owner, _)| (owner.client_id, owner.upstream_id))
    }

    /// Deletes an active refresh token by its digest (F8's replay
    /// containment extended to whatever refresh token an authorization-code
    /// redemption also minted).
    pub(crate) fn delete_by_digest(&self, digest: [u8; 32]) {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&digest);
    }

    /// Removes every *active* refresh token bound to `upstream_id` (session
    /// removal's referential cleanup, finding 6). `retired` is deliberately
    /// left untouched: reuse detection (R2) must outlive the session it
    /// belonged to, and it is already bounded on its own by
    /// [`RETIRED_REFRESH_TTL`]. Run only from
    /// `super::store::ProxyState::take_session`/`sweep_expired_sessions`,
    /// never the resolve path.
    pub(crate) fn remove_by_upstream(&self, upstream_id: &str) {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, (owner, _)| owner.upstream_id != upstream_id);
    }

    /// Every `client_id` with a currently unexpired *active* refresh token:
    /// the other half of the anti-eviction live set
    /// [`ProxyState::register_client`] passes to [`ClientRegistry::register`].
    /// Reads `current` only — a `retired` digest carries no `client_id` and
    /// a rotated-away token is not a live session; the session is
    /// represented by its current refresh entry.
    pub(crate) fn live_client_ids(&self) -> HashSet<String> {
        let now = Instant::now();
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(owner, _)| owner.client_id.clone())
            .collect()
    }
}

// --- the bundle shared by every oauth-proxy route and `get_mcp_server_info` (R7) ---

/// Every oauth-proxy store, constructed once by `RedmineMcp::new` so
/// `transport::http::router`'s route/middleware wiring and
/// `tools::meta::get_mcp_server_info`'s session counts read the very same
/// instances rather than two independent copies.
#[derive(Debug)]
pub(crate) struct ProxyState {
    pub(crate) registry: ClientRegistry,
    pub(crate) transactions: TransactionStore,
    pub(crate) codes: CodeStore,
    pub(crate) tokens: TokenStore,
    pub(crate) upstream_tokens: UpstreamStore,
    pub(crate) refresh_tokens: RefreshStore,
}

impl ProxyState {
    pub(crate) fn new() -> Self {
        Self {
            registry: ClientRegistry::new(),
            transactions: TransactionStore::new(),
            codes: CodeStore::new(),
            tokens: TokenStore::new(),
            upstream_tokens: UpstreamStore::new(),
            refresh_tokens: RefreshStore::new(),
        }
    }

    /// Registers a new DCR client: builds the union of every `client_id`
    /// currently holding a live proxy access or refresh token
    /// *before* the registry's own lock is taken, then delegates to
    /// [`ClientRegistry::register`] — each store's lock is released before
    /// the next is acquired, so no lock here is ever held across another.
    /// `None` means the registry is full even after sweeping reclaimable
    /// entries, or `OsRng` failed (C8); the caller turns either into a
    /// `503` with `Retry-After`.
    pub(crate) fn register_client(
        &self,
        redirect_uris: Vec<String>,
        client_name: Option<String>,
    ) -> Option<ClientRegistration> {
        let mut live = self.tokens.live_client_ids();
        live.extend(self.refresh_tokens.live_client_ids());
        self.registry.register(redirect_uris, client_name, &live)
    }

    /// Removes the session `upstream_id` names, along with every proxy
    /// access/refresh token bound to it (finding 6's referential cleanup):
    /// `UpstreamStore`, `TokenStore`, and `RefreshStore` each guard their
    /// own map and cannot reach across each other to do this themselves.
    /// `#[must_use]`: the returned set is what the caller needs to revoke
    /// upstream and purge from the introspection cache, work this module
    /// cannot do (it has no HTTP client and no `TokenVerifier`).
    #[must_use]
    pub(crate) fn take_session(&self, upstream_id: &str) -> Option<UpstreamTokenSet> {
        let set = self.upstream_tokens.take(upstream_id)?;
        self.tokens.remove_by_upstream(upstream_id);
        self.refresh_tokens.remove_by_upstream(upstream_id);
        Some(set)
    }

    /// Sweeps every session past its deadline, cross-purging each one's
    /// dependent proxy tokens the same way [`Self::take_session`] does.
    /// `#[must_use]`: the returned sets are what the caller needs to purge
    /// from the introspection cache and, for whichever carry a refresh
    /// token, revoke upstream (V1/V2/V5: only the refresh token is worth
    /// revoking on a sweep — see `oauth::proxy::endpoints`).
    #[must_use]
    pub(crate) fn sweep_expired_sessions(&self) -> Vec<UpstreamTokenSet> {
        self.upstream_tokens
            .sweep_expired()
            .into_iter()
            .map(|(upstream_id, set)| {
                self.tokens.remove_by_upstream(&upstream_id);
                self.refresh_tokens.remove_by_upstream(&upstream_id);
                set
            })
            .collect()
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

    /// No live tokens in any of these tests: an empty set stands in for
    /// [`ProxyState::register_client`]'s union whenever a test only cares
    /// about the idle-TTL half of the policy.
    fn no_live() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn register_returns_a_32_char_hex_client_id() {
        let registry = ClientRegistry::new();
        let registration = registry
            .register(vec!["http://localhost/cb".to_string()], None, &no_live())
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
                &no_live(),
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
        let a = registry
            .register(vec![], None, &no_live())
            .expect("should register");
        let b = registry
            .register(vec![], None, &no_live())
            .expect("should register");
        assert_ne!(a.client_id, b.client_id);
    }

    #[test]
    fn debug_prints_counts_not_registration_contents() {
        let registry = ClientRegistry::new();
        registry
            .register(
                vec!["https://secret-looking-host.example/cb".to_string()],
                None,
                &no_live(),
            )
            .expect("should register");
        let rendered = format!("{registry:?}");
        assert!(!rendered.contains("secret-looking-host"));
        assert!(rendered.contains("len"));
    }

    /// The anti-eviction property: a full registry of *recent* idle
    /// registrations refuses a new registration and evicts nothing — unlike
    /// the old LRU policy, no entry is ever sacrificed to make room.
    #[test]
    fn full_registry_of_recent_registrations_refuses_new_and_evicts_nothing() {
        let registry = ClientRegistry::new();
        let mut ids = Vec::new();
        for _ in 0..MAX_CLIENTS {
            ids.push(
                registry
                    .register(vec![], None, &no_live())
                    .expect("should register")
                    .client_id,
            );
        }
        assert_eq!(registry.len(), MAX_CLIENTS);

        assert!(registry.register(vec![], None, &no_live()).is_none());
        assert_eq!(registry.len(), MAX_CLIENTS);
        for id in &ids {
            assert!(registry.get(id).is_some(), "no entry should be evicted");
        }
    }

    /// A registration idle past [`CLIENT_IDLE_TTL`] with no live session is
    /// reclaimed under capacity pressure, and exactly that entry
    /// disappears.
    #[test]
    fn an_entry_idle_past_the_ttl_is_reclaimed_under_capacity_pressure() {
        let registry = ClientRegistry::new();
        let stale = registry
            .register(vec![], None, &no_live())
            .expect("should register");
        registry.rewind(&stale.client_id, CLIENT_IDLE_TTL + Duration::from_secs(1));
        for _ in 1..MAX_CLIENTS {
            registry
                .register(vec![], None, &no_live())
                .expect("should register");
        }
        assert_eq!(registry.len(), MAX_CLIENTS);

        let newest = registry
            .register(vec![], None, &no_live())
            .expect("the sweep should have made room");
        assert_eq!(registry.len(), MAX_CLIENTS);
        assert!(registry.get(&stale.client_id).is_none());
        assert!(registry.get(&newest.client_id).is_some());
    }

    /// `touch` resets the idle clock: an entry rewound past the TTL, then
    /// touched, survives capacity pressure.
    #[test]
    fn touch_resets_the_idle_clock_so_the_entry_survives_capacity_pressure() {
        let registry = ClientRegistry::new();
        let touched = registry
            .register(vec![], None, &no_live())
            .expect("should register");
        registry.rewind(&touched.client_id, CLIENT_IDLE_TTL + Duration::from_secs(1));
        registry.touch(&touched.client_id);
        for _ in 1..MAX_CLIENTS {
            registry
                .register(vec![], None, &no_live())
                .expect("should register");
        }
        assert_eq!(registry.len(), MAX_CLIENTS);

        assert!(
            registry.register(vec![], None, &no_live()).is_none(),
            "the touched entry should not have been reclaimed"
        );
        assert!(registry.get(&touched.client_id).is_some());
    }

    /// A client past the idle TTL that owns a live proxy access token
    /// survives capacity pressure, and becomes reclaimable once
    /// `take_session` removes that token — proving both the derived
    /// liveness and its release.
    #[test]
    fn a_client_with_a_live_access_token_survives_ttl_expiry_until_its_session_is_removed() {
        let proxy = ProxyState::new();
        let stale = proxy
            .registry
            .register(vec![], None, &no_live())
            .expect("should register");
        proxy
            .registry
            .rewind(&stale.client_id, CLIENT_IDLE_TTL + Duration::from_secs(1));

        let upstream_id = proxy
            .upstream_tokens
            .insert(UpstreamTokenSet {
                access: SecretString::from("access"),
                refresh: None,
                granted_scopes: vec![],
                expires_at: expires_after(Duration::from_mins(5)),
            })
            .expect("should insert");
        proxy
            .tokens
            .mint(
                stale.client_id.clone(),
                upstream_id.clone(),
                Duration::from_mins(5),
            )
            .expect("should mint");

        for _ in 1..MAX_CLIENTS {
            proxy
                .register_client(vec![], None)
                .expect("should register");
        }
        assert_eq!(proxy.registry.len(), MAX_CLIENTS);

        assert!(
            proxy.register_client(vec![], None).is_none(),
            "the live entry must survive capacity pressure"
        );
        assert!(proxy.registry.get(&stale.client_id).is_some());

        let _ = proxy.take_session(&upstream_id);
        let newest = proxy
            .register_client(vec![], None)
            .expect("the sweep should have made room once the session is gone");
        assert!(proxy.registry.get(&stale.client_id).is_none());
        assert!(proxy.registry.get(&newest.client_id).is_some());
    }

    /// A refresh-token-only client (no access token minted) is equally
    /// protected by the `RefreshStore` half of the live-client union.
    #[test]
    fn a_client_with_only_a_live_refresh_token_survives_ttl_expiry() {
        let proxy = ProxyState::new();
        let stale = proxy
            .registry
            .register(vec![], None, &no_live())
            .expect("should register");
        proxy
            .registry
            .rewind(&stale.client_id, CLIENT_IDLE_TTL + Duration::from_secs(1));
        proxy
            .refresh_tokens
            .mint(stale.client_id.clone(), "upstream-1".to_string())
            .expect("should mint");

        for _ in 1..MAX_CLIENTS {
            proxy
                .register_client(vec![], None)
                .expect("should register");
        }
        assert_eq!(proxy.registry.len(), MAX_CLIENTS);

        assert!(proxy.register_client(vec![], None).is_none());
        assert!(proxy.registry.get(&stale.client_id).is_some());
    }

    /// The caps every store above degrades under, reviewable together
    /// rather than scattered across the file.
    #[test]
    fn every_store_cap_is_documented_here() {
        assert_eq!(MAX_CLIENTS, 1000);
        assert_eq!(CLIENT_IDLE_TTL, Duration::from_hours(1));
        assert_eq!(MAX_ENTRIES, 10_000);
        assert_eq!(REFRESH_TTL, Duration::from_hours(720));
        assert_eq!(RETIRED_REFRESH_TTL, Duration::from_hours(24));
    }

    /// Driving 2x `MAX_ENTRIES` through `TokenStore` stays bounded and keeps
    /// the most recently minted entries alive.
    #[test]
    fn token_store_stays_bounded_under_pressure_and_keeps_live_entries() {
        let store = TokenStore::new();
        let mut survivors = Vec::new();
        for i in 0..MAX_ENTRIES * 2 {
            if let Some((token, _)) = store.mint(
                format!("client-{i}"),
                format!("upstream-{i}"),
                Duration::from_mins(5),
            ) {
                survivors.push(token);
            }
        }
        // At least the last mint (well within capacity by then) must have
        // succeeded and still resolve.
        let last = survivors.last().expect("at least one mint should succeed");
        assert!(store.resolve(last).is_some());
    }

    /// Same shape for `CodeStore`.
    #[test]
    fn code_store_stays_bounded_under_pressure_and_keeps_live_entries() {
        let store = CodeStore::new();
        let mut last_code = None;
        for i in 0..MAX_ENTRIES * 2 {
            let upstream = UpstreamTokenSet {
                access: SecretString::from(format!("access-{i}")),
                refresh: None,
                granted_scopes: vec![],
                expires_at: expires_after(Duration::from_mins(5)),
            };
            if let Some(code) = store.mint(
                format!("client-{i}"),
                "http://localhost/cb".to_string(),
                "challenge".to_string(),
                upstream,
            ) {
                last_code = Some(code);
            }
        }
        let code = last_code.expect("at least one mint should succeed");
        // A wrong redirect_uri is a `Mismatch`, not `Invalid`: the surviving
        // code is still present and bound, just not redeemed with these
        // (deliberately wrong) checks.
        assert!(matches!(
            store.redeem(&code, "client-x", "http://localhost/wrong", "v"),
            RedeemOutcome::Mismatch
        ));
    }

    /// Same shape for `TransactionStore`.
    #[test]
    fn transaction_store_stays_bounded_under_pressure_and_keeps_live_entries() {
        let store = TransactionStore::new();
        let mut last_id = None;
        for _ in 0..MAX_ENTRIES * 2 {
            if let Some(id) = store.create(Transaction {
                client_id: "client".to_string(),
                redirect_uri: "http://localhost/cb".to_string(),
                code_challenge: "challenge".to_string(),
                scopes: vec![],
                client_state: None,
                upstream_code_verifier: "verifier".to_string(),
            }) {
                last_id = Some(id);
            }
        }
        let id = last_id.expect("at least one create should succeed");
        assert!(store.take(&id).is_some());
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod refresh_store_tests {
    use super::*;

    #[test]
    fn mint_then_redeem_returns_the_bound_client_and_upstream_id() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        match store.redeem(&token) {
            RefreshOutcome::Ok {
                client_id,
                upstream_id,
                guard: _,
            } => {
                assert_eq!(client_id, "client-a");
                assert_eq!(upstream_id, "upstream-1");
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn unknown_token_is_invalid() {
        let store = RefreshStore::new();
        assert!(matches!(
            store.redeem("rup_rt_never-issued"),
            RefreshOutcome::Invalid
        ));
    }

    #[test]
    fn a_retired_token_is_still_redeemable_as_reused_not_invalid() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        store.retire(&token, "upstream-1".to_string());
        match store.redeem(&token) {
            RefreshOutcome::Reused { upstream_id } => assert_eq!(upstream_id, "upstream-1"),
            _ => panic!("expected Reused"),
        }
    }

    #[test]
    fn a_retired_token_no_longer_redeems_as_ok() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        store.retire(&token, "upstream-1".to_string());
        assert!(!matches!(store.redeem(&token), RefreshOutcome::Ok { .. }));
    }

    #[test]
    fn discard_removes_the_active_token_with_no_reuse_tracking() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        store.discard(&token);
        assert!(matches!(store.redeem(&token), RefreshOutcome::Invalid));
    }

    #[test]
    fn take_removes_and_returns_the_active_owner() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        let (client_id, upstream_id) = store.take(&token).expect("should be active");
        assert_eq!(client_id, "client-a");
        assert_eq!(upstream_id, "upstream-1");
        assert!(store.take(&token).is_none());
    }

    #[test]
    fn take_does_not_resolve_a_retired_token() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        store.retire(&token, "upstream-1".to_string());
        assert!(store.take(&token).is_none());
    }

    #[test]
    fn a_second_redeem_while_the_first_guard_is_alive_is_reused() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        let RefreshOutcome::Ok { guard, .. } = store.redeem(&token) else {
            panic!("expected Ok");
        };
        match store.redeem(&token) {
            RefreshOutcome::Reused { upstream_id } => assert_eq!(upstream_id, "upstream-1"),
            _ => panic!("expected Reused while the first guard is alive"),
        }
        drop(guard);
    }

    #[test]
    fn dropping_the_guard_without_retiring_makes_the_token_redeemable_again() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        {
            let RefreshOutcome::Ok { guard, .. } = store.redeem(&token) else {
                panic!("expected Ok");
            };
            drop(guard);
        }
        assert!(matches!(store.redeem(&token), RefreshOutcome::Ok { .. }));
    }

    #[test]
    fn retiring_while_the_guard_is_alive_then_dropping_it_stays_reused() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        let RefreshOutcome::Ok { guard, .. } = store.redeem(&token) else {
            panic!("expected Ok");
        };
        store.retire(&token, "upstream-1".to_string());
        // The guard's entry is already gone, so its `Drop` must be a
        // no-op: it must not resurrect the retired token as `Active`.
        drop(guard);
        match store.redeem(&token) {
            RefreshOutcome::Reused { upstream_id } => assert_eq!(upstream_id, "upstream-1"),
            _ => panic!("expected Reused; the guard must not have undone the retire"),
        }
    }

    #[test]
    fn discarding_while_the_guard_is_alive_then_dropping_it_stays_invalid() {
        let store = RefreshStore::new();
        let (token, _) = store
            .mint("client-a".to_string(), "upstream-1".to_string())
            .expect("should mint");
        let RefreshOutcome::Ok { guard, .. } = store.redeem(&token) else {
            panic!("expected Ok");
        };
        store.discard(&token);
        drop(guard);
        assert!(matches!(store.redeem(&token), RefreshOutcome::Invalid));
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod upstream_store_tests {
    use super::*;

    fn upstream_set(access: &str) -> UpstreamTokenSet {
        UpstreamTokenSet {
            access: SecretString::from(access.to_string()),
            refresh: None,
            granted_scopes: vec![],
            expires_at: expires_after(Duration::from_mins(5)),
        }
    }

    #[test]
    fn replace_on_a_present_id_updates_in_place_and_returns_ok() {
        let store = UpstreamStore::new();
        let id = store.insert(upstream_set("old")).expect("should insert");
        assert_eq!(store.len(), 1);

        let result = store.replace(&id, upstream_set("new"));
        assert!(result.is_ok());
        assert_eq!(store.len(), 1);
        let access = store.access_token(&id).expect("should still resolve");
        assert_eq!(secrecy::ExposeSecret::expose_secret(&access), "new");
    }

    #[test]
    fn replace_on_an_absent_id_returns_err_and_inserts_nothing() {
        let store = UpstreamStore::new();
        assert_eq!(store.len(), 0);

        let result = store.replace("never-inserted", upstream_set("orphaned"));
        assert!(result.is_err());
        assert_eq!(store.len(), 0);
    }

    fn refreshable_upstream_set(access: &str, refresh: &str) -> UpstreamTokenSet {
        UpstreamTokenSet {
            access: SecretString::from(access.to_string()),
            refresh: Some(SecretString::from(refresh.to_string())),
            granted_scopes: vec![],
            // Deliberately short: proves the deadline ignores this once a
            // refresh token is present (V2, V5).
            expires_at: expires_after(Duration::from_millis(1)),
        }
    }

    /// Already at its deadline as soon as it is constructed: every
    /// `Instant::now()` check after a short sleep sees it as expired.
    fn already_expired_upstream_set(access: &str) -> UpstreamTokenSet {
        UpstreamTokenSet {
            access: SecretString::from(access.to_string()),
            refresh: None,
            granted_scopes: vec!["view_project".to_string()],
            expires_at: Instant::now(),
        }
    }

    #[test]
    fn session_deadline_is_the_access_expiry_without_a_refresh_token() {
        let set = upstream_set("access-only");
        assert_eq!(UpstreamStore::session_deadline(&set), set.expires_at);
    }

    #[test]
    fn session_deadline_is_refresh_ttl_when_a_refresh_token_is_present() {
        let set = refreshable_upstream_set("access", "refresh");
        let deadline = UpstreamStore::session_deadline(&set);
        // Far beyond the access token's own (1ms) expiry: a refresh token
        // keeps the session alive for REFRESH_TTL regardless of how soon
        // its access token expires.
        assert!(deadline > set.expires_at + Duration::from_secs(60));
    }

    #[test]
    fn getters_fail_closed_past_the_deadline_but_take_still_returns() {
        let store = UpstreamStore::new();
        let id = store
            .insert(already_expired_upstream_set("expiring"))
            .expect("should insert");
        std::thread::sleep(Duration::from_millis(5));

        assert!(store.access_token(&id).is_none());
        assert!(store.refresh_token(&id).is_none());
        assert!(store.granted_scopes(&id).is_none());
        assert_eq!(store.len(), 0);

        let taken = store.take(&id).expect("take ignores the deadline");
        assert_eq!(
            secrecy::ExposeSecret::expose_secret(&taken.access),
            "expiring"
        );
    }

    #[test]
    fn replace_renews_the_deadline_on_a_live_entry() {
        let store = UpstreamStore::new();
        let id = store.insert(upstream_set("old")).expect("should insert");

        // Renewed with a refresh-bearing set: the deadline moves from the
        // original 5-minute access expiry out to `REFRESH_TTL`.
        let result = store.replace(&id, refreshable_upstream_set("new", "refresh"));
        assert!(result.is_ok());
        assert!(store.refresh_token(&id).is_some());
    }

    #[test]
    fn replace_rejects_an_entry_already_past_its_deadline() {
        let store = UpstreamStore::new();
        let id = store
            .insert(already_expired_upstream_set("expiring"))
            .expect("should insert");
        std::thread::sleep(Duration::from_millis(5));

        let result = store.replace(&id, upstream_set("orphaned"));
        assert!(
            result.is_err(),
            "an in-flight refresh must not resurrect a session already past its deadline"
        );
    }

    #[test]
    fn sweep_expired_returns_exactly_the_expired_entries() {
        let store = UpstreamStore::new();
        // Inserted first: a later insert's own sweep would otherwise prune
        // this one before the explicit `sweep_expired` call below gets to.
        let live_id = store.insert(upstream_set("live")).expect("should insert");
        let expired_id = store
            .insert(already_expired_upstream_set("expired"))
            .expect("should insert");
        std::thread::sleep(Duration::from_millis(5));

        let swept = store.sweep_expired();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].0, expired_id);
        assert_eq!(
            secrecy::ExposeSecret::expose_secret(&swept[0].1.access),
            "expired"
        );

        // The live entry is untouched.
        assert_eq!(store.len(), 1);
        assert!(store.access_token(&live_id).is_some());
    }

    #[test]
    fn len_excludes_expired_entries() {
        let store = UpstreamStore::new();
        // Inserted first, same reasoning as the `sweep_expired` test above.
        store.insert(upstream_set("live")).expect("should insert");
        store
            .insert(already_expired_upstream_set("expired"))
            .expect("should insert");
        std::thread::sleep(Duration::from_millis(5));

        assert_eq!(store.len(), 1);
    }

    /// Same shape as every other store's capacity test: bounded under
    /// pressure. Every fill entry uses a 5-minute TTL (far longer than this
    /// test runs) so none self-expires mid-loop.
    #[test]
    fn insert_is_refused_once_max_entries_live_sessions_exist() {
        let store = UpstreamStore::new();
        for i in 0..MAX_ENTRIES {
            store
                .insert(upstream_set(&format!("access-{i}")))
                .expect("should insert while under capacity");
        }
        assert_eq!(store.len(), MAX_ENTRIES);
        assert!(store.insert(upstream_set("one-too-many")).is_none());
    }

    /// Populates the map directly (bypassing `insert`'s own per-call sweep)
    /// so it is deterministically full of already-expired entries, then
    /// proves a single `insert` call's sweep reclaims all of that room —
    /// the behaviour every sibling store already had and this one gains.
    #[test]
    fn insert_succeeds_again_once_a_sweep_reclaims_expired_sessions() {
        let store = UpstreamStore::new();
        {
            let mut inner = store.inner.lock().expect("lock should not be poisoned");
            for i in 0..MAX_ENTRIES {
                inner.insert(
                    format!("existing-{i}"),
                    (
                        already_expired_upstream_set(&format!("access-{i}")),
                        Instant::now(),
                    ),
                );
            }
        }
        assert_eq!(store.len(), 0, "every pre-populated entry is expired");

        let id = store
            .insert(upstream_set("fits-after-sweep"))
            .expect("a sweep on insert should reclaim the expired entries");
        assert!(store.access_token(&id).is_some());
        assert_eq!(store.len(), 1);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod proxy_state_tests {
    use super::*;

    fn upstream_set_with_refresh(access: &str, refresh: &str) -> UpstreamTokenSet {
        UpstreamTokenSet {
            access: SecretString::from(access.to_string()),
            refresh: Some(SecretString::from(refresh.to_string())),
            granted_scopes: vec![],
            expires_at: expires_after(Duration::from_mins(5)),
        }
    }

    fn already_expired_upstream_set(access: &str) -> UpstreamTokenSet {
        UpstreamTokenSet {
            access: SecretString::from(access.to_string()),
            refresh: None,
            granted_scopes: vec![],
            expires_at: Instant::now(),
        }
    }

    #[test]
    fn take_session_removes_the_upstream_set_and_every_bound_proxy_token() {
        let proxy = ProxyState::new();
        let upstream_id = proxy
            .upstream_tokens
            .insert(upstream_set_with_refresh("access", "refresh"))
            .expect("should insert");
        let (access_token, _) = proxy
            .tokens
            .mint(
                "client".to_string(),
                upstream_id.clone(),
                Duration::from_mins(5),
            )
            .expect("should mint");
        let (refresh_token, _) = proxy
            .refresh_tokens
            .mint("client".to_string(), upstream_id.clone())
            .expect("should mint");

        let taken = proxy
            .take_session(&upstream_id)
            .expect("session should exist");
        assert_eq!(
            secrecy::ExposeSecret::expose_secret(&taken.access),
            "access"
        );

        assert!(proxy.upstream_tokens.access_token(&upstream_id).is_none());
        assert!(proxy.tokens.resolve(&access_token).is_none());
        assert!(proxy.refresh_tokens.take(&refresh_token).is_none());
    }

    #[test]
    fn take_session_leaves_a_retired_refresh_digest_reusable_for_replay_detection() {
        let proxy = ProxyState::new();
        let upstream_id = proxy
            .upstream_tokens
            .insert(upstream_set_with_refresh("access", "refresh"))
            .expect("should insert");
        let (refresh_token, _) = proxy
            .refresh_tokens
            .mint("client".to_string(), upstream_id.clone())
            .expect("should mint");
        proxy
            .refresh_tokens
            .retire(&refresh_token, upstream_id.clone());

        proxy
            .take_session(&upstream_id)
            .expect("session should exist");

        // R2's reuse detection must outlive the session it belonged to.
        assert!(matches!(
            proxy.refresh_tokens.redeem(&refresh_token),
            RefreshOutcome::Reused { .. }
        ));
    }

    #[test]
    fn sweep_expired_sessions_cross_purges_only_the_expired_sessions_tokens() {
        let proxy = ProxyState::new();
        // Inserted first: inserting the expired session afterward would
        // otherwise prune this one's own already-past deadline as a side
        // effect before the explicit sweep below gets to it.
        let live_id = proxy
            .upstream_tokens
            .insert(upstream_set_with_refresh("live-access", "live-refresh"))
            .expect("should insert");
        let (live_token, _) = proxy
            .tokens
            .mint(
                "client".to_string(),
                live_id.clone(),
                Duration::from_mins(5),
            )
            .expect("should mint");

        let expired_id = proxy
            .upstream_tokens
            .insert(already_expired_upstream_set("expired-access"))
            .expect("should insert");
        let (expired_token, _) = proxy
            .tokens
            .mint(
                "client".to_string(),
                expired_id.clone(),
                Duration::from_mins(5),
            )
            .expect("should mint");

        std::thread::sleep(Duration::from_millis(5));

        let swept = proxy.sweep_expired_sessions();
        assert_eq!(swept.len(), 1);
        assert_eq!(
            secrecy::ExposeSecret::expose_secret(&swept[0].access),
            "expired-access"
        );

        assert!(proxy.tokens.resolve(&expired_token).is_none());
        assert!(proxy.tokens.resolve(&live_token).is_some());
        assert!(proxy.upstream_tokens.access_token(&live_id).is_some());
    }
}
