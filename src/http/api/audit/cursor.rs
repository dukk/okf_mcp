//! A team owner's audit cursor (issue #430). `audit_entries.id` is one
//! global sequence, so a raw id in the owner view would let an owner
//! subtract two of their own entries' ids and learn how many entries every
//! other team wrote in between. The owner view therefore shows no id at
//! all, and its cursor is the next position (time, store and entry id, see
//! [`AuditCursor`]) and the owner's audit team sealed with ChaCha20-Poly1305 under a server key
//! ([`AUDIT_CURSOR`](crate::store::server_keys::AUDIT_CURSOR)) and a fresh random nonce.
//!
//! Why encryption, not a MAC: a MAC proves a token is genuine but leaves
//! the id inside it readable. Why a random nonce: two cursors for the same
//! position look unrelated, so an owner cannot compare or order cursors
//! either; only the server can open one. The sealed audit team binds a
//! cursor to the team it was issued for, so it never pages another team.
//!
//! Every cursor that does not open (tampered, truncated, issued for another
//! team, sealed under a rotated key, or a raw id) is the same 400, with the
//! same text, so the refusal says nothing about why.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};

use crate::error::OkfError;
use crate::store::audit::AuditCursor;
use crate::store::server_keys::KEY_LEN;

/// Bound into every cursor (associated data), so nothing else sealed under
/// the same key, now or in a later format, opens as a cursor.
const CONTEXT: &[u8] = b"okf-mcp audit owner cursor v2";

/// The sealed plaintext's fixed part: the audit team id, the time and the
/// entry id, eight bytes each. The store's name follows, up to
/// [`MAX_STORE_LEN`] bytes.
const FIXED_LEN: usize = 24;

/// The longest store name a cursor holds; a longer one is not a store.
const MAX_STORE_LEN: usize = 63;

/// The authentication tag's length (ChaCha20-Poly1305's).
const TAG_LEN: usize = 16;

/// The refusal for any cursor that does not open.
///
/// # Returns
/// A 400 naming the `after` parameter.
#[tracing::instrument(level = "trace", skip_all)]
fn refused() -> OkfError {
    OkfError::InvalidPath {
        path: "after".to_string(),
        reason: "not a cursor this audit view issued; load the first page again".to_string(),
    }
}

/// The AEAD key.
///
/// # Arguments
/// * `key` - the server key's bytes.
///
/// # Returns
/// The key, ready to seal and open.
#[tracing::instrument(level = "trace", skip_all)]
fn aead(key: &[u8; KEY_LEN]) -> LessSafeKey {
    // ChaCha20-Poly1305 takes exactly 32 key bytes, which `[u8; KEY_LEN]`
    // guarantees, so `new` cannot fail.
    LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, key).expect("a 32-byte ChaCha20-Poly1305 key"),
    )
}

/// Seals the cursor that continues after `position` in `audit_team`'s view.
///
/// # Arguments
/// * `key` - the [`AUDIT_CURSOR`](crate::store::server_keys::AUDIT_CURSOR) key.
/// * `audit_team` - the owner's `audit_teams` id (`Viewer::Owner`).
/// * `position` - the last entry of the page.
///
/// # Returns
/// The cursor: unpadded base64url of the nonce, the ciphertext and the tag.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn seal(
    key: &[u8; KEY_LEN],
    audit_team: i64,
    position: &AuditCursor,
) -> Result<String, OkfError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    let (at_micros, store, id) = position.parts();
    let mut sealed = Vec::with_capacity(FIXED_LEN + store.len() + TAG_LEN);
    sealed.extend_from_slice(&audit_team.to_be_bytes());
    sealed.extend_from_slice(&at_micros.to_be_bytes());
    sealed.extend_from_slice(&id.to_be_bytes());
    sealed.extend_from_slice(store.as_bytes());
    aead(key)
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(CONTEXT),
            &mut sealed,
        )
        // Sealing fails only for a plaintext of gigabytes.
        .expect("a short cursor seals");
    let mut token = nonce.to_vec();
    token.extend_from_slice(&sealed);
    Ok(URL_SAFE_NO_PAD.encode(token))
}

/// Opens a cursor that [`seal`] issued for `audit_team`.
///
/// # Arguments
/// * `key` - the [`AUDIT_CURSOR`](crate::store::server_keys::AUDIT_CURSOR) key.
/// * `audit_team` - the asking owner's `audit_teams` id.
/// * `token` - the `after` parameter as sent.
///
/// # Returns
/// The position the next page starts after.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400), the same for every reason: not
/// base64url, the wrong length, a failed tag (tampered, or sealed under
/// another key), or sealed for another team.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn open(
    key: &[u8; KEY_LEN],
    audit_team: i64,
    token: &str,
) -> Result<AuditCursor, OkfError> {
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| refused())?;
    let overhead = NONCE_LEN + FIXED_LEN + TAG_LEN;
    if bytes.len() < overhead || bytes.len() > overhead + MAX_STORE_LEN {
        return Err(refused());
    }
    let (nonce, sealed) = bytes.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| refused())?;
    let mut sealed = sealed.to_vec();
    let plain = aead(key)
        .open_in_place(nonce, Aad::from(CONTEXT), &mut sealed)
        .map_err(|_| refused())?;
    // `split_at` and `try_into` turn the fixed bytes into three 8-byte
    // arrays; the length check above makes every conversion succeed.
    let (team, rest) = plain.split_at(8);
    let (at, rest) = rest.split_at(8);
    let (id, store) = rest.split_at(8);
    let team = i64::from_be_bytes(team.try_into().map_err(|_| refused())?);
    let at = i64::from_be_bytes(at.try_into().map_err(|_| refused())?);
    let id = i64::from_be_bytes(id.try_into().map_err(|_| refused())?);
    if team != audit_team {
        return Err(refused());
    }
    let store = std::str::from_utf8(store).map_err(|_| refused())?;
    AuditCursor::from_parts(at, store, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [7; KEY_LEN];

    fn position() -> AuditCursor {
        AuditCursor::from_parts(1_700_000_000_123_456, "main", 1234).unwrap()
    }

    #[test]
    fn a_cursor_opens_for_its_own_team_and_key_only() {
        let token = seal(&KEY, 5, &position()).unwrap();

        assert_eq!(open(&KEY, 5, &token).unwrap(), position());
        for refused in [
            open(&KEY, 6, &token),
            open(&[8; KEY_LEN], 5, &token),
            open(&KEY, 5, "1234"),
            open(&KEY, 5, ""),
            open(&KEY, 5, "not base64!"),
            open(&KEY, 5, &token[..token.len() - 1]),
        ] {
            let error = refused.unwrap_err();
            assert!(
                matches!(error, OkfError::InvalidPath { ref path, .. } if path == "after"),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_account_database_position_round_trips_with_no_store() {
        let account = AuditCursor::from_parts(5, "", 9).unwrap();

        let token = seal(&KEY, 5, &account).unwrap();

        assert_eq!(open(&KEY, 5, &token).unwrap(), account);
    }

    #[test]
    fn every_flipped_byte_is_refused() {
        let token = seal(&KEY, 5, &position()).unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        for at in 0..bytes.len() {
            let mut tampered = bytes.clone();
            tampered[at] ^= 1;
            let tampered = URL_SAFE_NO_PAD.encode(tampered);
            assert!(open(&KEY, 5, &tampered).is_err(), "byte {at} flipped");
        }
    }

    #[test]
    fn two_cursors_for_the_same_place_share_nothing_readable() {
        let first = seal(&KEY, 5, &position()).unwrap();
        let second = seal(&KEY, 5, &position()).unwrap();

        assert_ne!(first, second, "a fresh nonce every time");
        assert!(!first.contains("1234"));
        assert!(!first.contains("main"));
    }
}
