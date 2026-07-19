use anyhow::{anyhow, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE: &str = "qa_session";
pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_CHARS: usize = 256;

fn argon2() -> Result<Argon2<'static>> {
    let params = Params::new(19 * 1024, 2, 1, None)
        .map_err(|error| anyhow!("invalid Argon2 parameters: {error}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn validate_username(username: &str) -> Result<String> {
    let username = username.trim();
    if !(3..=64).contains(&username.len())
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!(
            "用户名必须为 3-64 位，只能包含字母、数字、点、横线和下划线"
        ));
    }
    Ok(username.to_ascii_lowercase())
}

pub fn validate_password(password: &str) -> Result<()> {
    let length = password.chars().count();
    if !(MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&length) {
        return Err(anyhow!(
            "密码长度必须为 {MIN_PASSWORD_CHARS}-{MAX_PASSWORD_CHARS} 个字符"
        ));
    }
    if password.trim().is_empty() {
        return Err(anyhow!("密码不能只包含空白字符"));
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String> {
    validate_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    argon2()?
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow!("password hashing failed: {error}"))
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    let Ok(hash) = PasswordHash::new(encoded) else {
        return false;
    };
    argon2()
        .and_then(|argon2| {
            argon2
                .verify_password(password.as_bytes(), &hash)
                .map_err(|error| anyhow!(error.to_string()))
        })
        .is_ok()
}

pub fn new_session_token() -> String {
    use argon2::password_hash::rand_core::RngCore;

    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn session_token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

pub fn cookie_value(token: &str, max_age_seconds: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{secure}"
    )
}

pub fn clear_cookie_value(secure: bool) -> String {
    cookie_value("", 0, secure)
}

pub fn cookie_token(cookie_header: &str) -> Option<&str> {
    cookie_header.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == SESSION_COOKIE && !value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::{cookie_token, hash_password, session_token_hash, verify_password};

    #[test]
    fn hashes_and_verifies_passwords() {
        let hash = hash_password("correct-horse-battery").unwrap();
        assert!(verify_password("correct-horse-battery", &hash));
        assert!(!verify_password("wrong-password-value", &hash));
    }

    #[test]
    fn extracts_cookie_and_hashes_session_token() {
        assert_eq!(
            cookie_token("theme=dark; qa_session=secret-token; mode=full"),
            Some("secret-token")
        );
        assert_eq!(session_token_hash("token"), session_token_hash("token"));
        assert_ne!(session_token_hash("token"), session_token_hash("other"));
    }
}
