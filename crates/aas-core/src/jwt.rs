//! `decodeJwtClaims` — base64url-decode the payload segment, JSON-parse, no verification.
//! Mirrors asx `utils/jwt.ts`.

use base64::Engine;
use serde_json::Value;

/// Decode the claims (2nd segment) of a JWT. Returns `None` on any malformation.
pub fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    // Node `Buffer.from(x, 'base64url')` tolerates missing padding; URL_SAFE_NO_PAD matches,
    // and we also strip any stray padding to be safe.
    let trimmed = payload.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Convenience: read a string claim.
pub fn claim_str<'a>(claims: &'a Value, key: &str) -> Option<&'a str> {
    claims.get(key).and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn make_jwt(payload: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(payload).unwrap());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn decodes_claims() {
        let jwt = make_jwt(&serde_json::json!({"email": "a@b.com", "exp": 123}));
        let c = decode_jwt_claims(&jwt).unwrap();
        assert_eq!(claim_str(&c, "email"), Some("a@b.com"));
        assert_eq!(c.get("exp").and_then(|v| v.as_i64()), Some(123));
    }

    #[test]
    fn bad_input_is_none() {
        assert!(decode_jwt_claims("").is_none());
        assert!(decode_jwt_claims("onlyonesegment").is_none());
        assert!(decode_jwt_claims("a.!!!.c").is_none());
    }
}
