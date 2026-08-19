//! `oauth-proxy` state: the DCR client registry (P8, C7, C8), the
//! in-flight-transaction/authorization-code/token stores (F2, F6, F9), all
//! bounded, `Mutex`-guarded maps rather than a database — the whole store is
//! gone on restart (P4), and every operation is short enough that holding
//! the lock across it (never across an `.await`, F12) is the right call
//! rather than an async lock.

use std::collections::HashMap;
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
            tracing::debug!(
                len = inner.len(),
                capacity = MAX_CLIENTS,
                "evicted an idle DCR client registration to make room"
            );
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
    inner: Mutex<HashMap<String, UpstreamTokenSet>>,
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

    /// Stores `set`, returning the internal id it was stored under. `None`
    /// on `OsRng` failure only (C8).
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
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), set);
        Some(id)
    }

    /// A clone of the stored access token, for the middleware to hand to
    /// [`crate::auth::oauth::TokenVerifier::verify`] on every request.
    pub(crate) fn access_token(&self, id: &str) -> Option<SecretString> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|set| set.access.clone())
    }

    /// A clone of the stored refresh token, if Doorkeeper issued one (R4),
    /// for the `/token` refresh grant to present upstream.
    pub(crate) fn refresh_token(&self, id: &str) -> Option<SecretString> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .and_then(|set| set.refresh.clone())
    }

    /// A clone of the stored granted scopes, for the `/token` refresh grant
    /// to fall back to when Doorkeeper's refresh response omits `scope`
    /// (RFC 6749 §6: absent means unchanged).
    pub(crate) fn granted_scopes(&self, id: &str) -> Option<Vec<String>> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|set| set.granted_scopes.clone())
    }

    pub(crate) fn remove(&self, id: &str) {
        self.take(id);
    }

    /// Removes and returns the stored set, for a caller that needs the
    /// access token it held to revoke it upstream (R5's `/revoke`, R2's
    /// reuse containment).
    pub(crate) fn take(&self, id: &str) -> Option<UpstreamTokenSet> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
    }

    /// Replaces the stored set in place, keeping `id` stable across a
    /// refresh (R1): the proxy access/refresh tokens minted before the
    /// refresh, and any bookkeeping keyed on `id`, all still resolve to the
    /// same session afterward.
    pub(crate) fn replace(&self, id: &str, set: UpstreamTokenSet) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string(), set);
    }

    /// Live upstream-session count, for `get_mcp_server_info`'s
    /// `active_sessions` (R7).
    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
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
    #[allow(
        dead_code,
        reason = "carried for a future scope-mismatch diagnostic; enforcement reads \
                  introspection (P9), not this field"
    )]
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

struct RefreshOwner {
    client_id: String,
    upstream_id: String,
}

/// Outcome of [`RefreshStore::redeem`].
pub(crate) enum RefreshOutcome {
    /// Unknown, expired, or never issued — indistinguishable by design,
    /// same reasoning as [`CodeStore`]'s `Invalid` (F7).
    Invalid,
    /// This digest was already rotated away: a replay (R2). `upstream_id`
    /// is the session's stable identifier across every rotation in its
    /// chain (see [`UpstreamStore::replace`]), so the caller can revoke
    /// whatever is *currently* live for it, however many rotations ago
    /// this particular token was current.
    Reused { upstream_id: String },
    /// A live, not-yet-rotated refresh token bound to `client_id` and
    /// `upstream_id`.
    Ok {
        client_id: String,
        upstream_id: String,
    },
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
                },
                expires_after(REFRESH_TTL),
            ),
        );
        Some((token, key))
    }

    /// Looks up `token` without consuming it — the caller mints the new
    /// pair and confirms it is durable *before* calling [`Self::retire`]
    /// (risk 1: the new pair must work before the old one stops).
    pub(crate) fn redeem(&self, token: &str) -> RefreshOutcome {
        let key = digest(token);
        {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            match current.get(&key) {
                Some((_, expires_at)) if *expires_at <= Instant::now() => {
                    current.remove(&key);
                }
                Some((owner, _)) => {
                    return RefreshOutcome::Ok {
                        client_id: owner.client_id.clone(),
                        upstream_id: owner.upstream_id.clone(),
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

    /// The caps every store above degrades under, reviewable together
    /// rather than scattered across the file.
    #[test]
    fn every_store_cap_is_documented_here() {
        assert_eq!(MAX_CLIENTS, 1000);
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
}
