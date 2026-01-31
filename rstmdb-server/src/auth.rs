//! Token-based authentication.
//!
//! Tokens are validated against SHA-256 hashes stored in configuration.
//! This avoids storing plaintext tokens while allowing simple bearer auth.

use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Validates bearer tokens against pre-configured hashes.
#[derive(Debug, Clone)]
pub struct TokenValidator {
    /// Set of valid token hashes (SHA-256 hex strings).
    valid_hashes: HashSet<String>,
}

impl TokenValidator {
    /// Creates a new validator with the given token hashes.
    pub fn new(hashes: impl IntoIterator<Item = String>) -> Self {
        Self {
            valid_hashes: hashes.into_iter().collect(),
        }
    }

    /// Returns whether any tokens are configured.
    pub fn has_tokens(&self) -> bool {
        !self.valid_hashes.is_empty()
    }

    /// Returns the number of configured tokens.
    pub fn token_count(&self) -> usize {
        self.valid_hashes.len()
    }

    /// Validates a plaintext token by hashing and comparing.
    pub fn validate(&self, token: &str) -> bool {
        if self.valid_hashes.is_empty() {
            return false;
        }
        let hash = Self::hash_token(token);
        self.valid_hashes.contains(&hash)
    }

    /// Hashes a token using SHA-256, returning a lowercase hex string.
    pub fn hash_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_token() {
        let hash = TokenValidator::hash_token("test-token");
        // SHA-256 produces 64 hex characters
        assert_eq!(hash.len(), 64);
        // Hash should be consistent
        assert_eq!(hash, TokenValidator::hash_token("test-token"));
        // Different tokens produce different hashes
        assert_ne!(hash, TokenValidator::hash_token("other-token"));
    }

    #[test]
    fn test_validate_correct_token() {
        let token = "my-secret-token";
        let hash = TokenValidator::hash_token(token);

        let validator = TokenValidator::new(vec![hash]);
        assert!(validator.validate(token));
    }

    #[test]
    fn test_validate_wrong_token() {
        let hash = TokenValidator::hash_token("correct-token");
        let validator = TokenValidator::new(vec![hash]);

        assert!(!validator.validate("wrong-token"));
    }

    #[test]
    fn test_validate_no_tokens_configured() {
        let validator = TokenValidator::new(Vec::<String>::new());
        assert!(!validator.has_tokens());
        assert!(!validator.validate("any-token"));
    }

    #[test]
    fn test_multiple_tokens() {
        let token1 = "token-one";
        let token2 = "token-two";
        let hashes = vec![
            TokenValidator::hash_token(token1),
            TokenValidator::hash_token(token2),
        ];

        let validator = TokenValidator::new(hashes);
        assert_eq!(validator.token_count(), 2);
        assert!(validator.validate(token1));
        assert!(validator.validate(token2));
        assert!(!validator.validate("token-three"));
    }

    #[test]
    fn test_empty_token() {
        let hash = TokenValidator::hash_token("");
        let validator = TokenValidator::new(vec![hash]);
        assert!(validator.validate(""));
        assert!(!validator.validate("non-empty"));
    }

    #[test]
    fn test_case_sensitivity() {
        let hash = TokenValidator::hash_token("MyToken");
        let validator = TokenValidator::new(vec![hash]);
        assert!(validator.validate("MyToken"));
        assert!(!validator.validate("mytoken"));
        assert!(!validator.validate("MYTOKEN"));
    }
}
