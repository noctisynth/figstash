//! Personal access token discovery and system keyring storage.

use figstash_core::{AppError, AppResult, CredentialKind, ErrorCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde_json::json;
use std::fmt::{Debug, Formatter};

const KEYRING_SERVICE: &str = "dev.figstash.cli";
const KEYRING_USER: &str = "figma-personal-access-token";

/// Where the current credential was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// Current process `FIGMA_TOKEN` environment variable.
    Environment,
    /// Current user's system keyring.
    Keyring,
}

/// Explicitly typed secret credential with redacted debug output.
pub struct Credential {
    kind: CredentialKind,
    source: CredentialSource,
    secret: SecretString,
}

impl Credential {
    /// Creates a personal access token credential.
    ///
    /// # Errors
    ///
    /// Returns `auth_missing` when the supplied secret is empty.
    pub fn personal_access_token(
        secret: impl Into<String>,
        source: CredentialSource,
    ) -> AppResult<Self> {
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::AuthMissing,
                "The personal access token is empty.",
            ));
        }
        Ok(Self {
            kind: CredentialKind::Pat,
            source,
            secret: SecretString::from(secret),
        })
    }

    /// Returns the explicit credential kind.
    #[must_use]
    pub const fn kind(&self) -> CredentialKind {
        self.kind
    }

    /// Returns the credential source.
    #[must_use]
    pub const fn source(&self) -> CredentialSource {
        self.source
    }

    pub(crate) fn expose(&self) -> &str {
        self.secret.expose_secret()
    }
}

impl Debug for Credential {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credential")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Minimal keyring boundary used by auth commands and tests.
pub trait KeyringBackend {
    /// Loads the stored PAT, returning `None` when no entry exists.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the system credential backend fails.
    fn load(&self) -> AppResult<Option<String>>;
    /// Replaces the stored PAT.
    ///
    /// # Errors
    ///
    /// Returns an auth or storage error when the PAT cannot be stored.
    fn store(&self, secret: &str) -> AppResult<()>;
    /// Deletes the stored PAT, returning whether an entry existed.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the credential backend fails.
    fn clear(&self) -> AppResult<bool>;
}

/// OS-native keyring implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemKeyring;

impl SystemKeyring {
    fn entry() -> AppResult<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(keyring_failure)
    }
}

impl KeyringBackend for SystemKeyring {
    fn load(&self) -> AppResult<Option<String>> {
        match Self::entry()?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_failure(error)),
        }
    }

    fn store(&self, secret: &str) -> AppResult<()> {
        if secret.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::AuthMissing,
                "The personal access token is empty.",
            ));
        }
        Self::entry()?.set_password(secret).map_err(keyring_failure)
    }

    fn clear(&self) -> AppResult<bool> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(keyring_failure(error)),
        }
    }
}

/// PAT source resolver with the fixed environment-before-keyring precedence.
#[derive(Debug, Clone)]
pub struct PatProvider<K> {
    keyring: K,
}

impl<K> PatProvider<K>
where
    K: KeyringBackend,
{
    /// Creates a PAT provider.
    pub const fn new(keyring: K) -> Self {
        Self { keyring }
    }

    /// Resolves `FIGMA_TOKEN` first and the system keyring second.
    ///
    /// # Errors
    ///
    /// Returns `auth_missing` when absent or a storage error on keyring failure.
    pub fn resolve(&self) -> AppResult<Credential> {
        if let Ok(secret) = std::env::var("FIGMA_TOKEN") {
            return Credential::personal_access_token(secret, CredentialSource::Environment);
        }
        if let Some(secret) = self.keyring.load()? {
            return Credential::personal_access_token(secret, CredentialSource::Keyring);
        }
        Err(AppError::new(
            ErrorCode::AuthMissing,
            "No Figma personal access token is configured.",
        )
        .with_details(json!({
            "sourcesChecked": ["FIGMA_TOKEN", "system_keyring"],
            "suggestedCommand": "figstash auth set --stdin"
        })))
    }

    /// Stores a PAT in the system keyring.
    ///
    /// # Errors
    ///
    /// Returns an auth or storage error when the PAT cannot be stored.
    pub fn store(&self, secret: String) -> AppResult<()> {
        let secret = SecretString::from(secret);
        if secret.expose_secret().trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::AuthMissing,
                "The personal access token is empty.",
            ));
        }
        self.keyring.store(secret.expose_secret())
    }

    /// Clears the PAT from the system keyring.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the keyring operation fails.
    pub fn clear(&self) -> AppResult<bool> {
        self.keyring.clear()
    }

    /// Reports a configured source without exposing token content.
    ///
    /// # Errors
    ///
    /// Returns a storage error when keyring inspection fails.
    pub fn status(&self) -> AppResult<Option<CredentialSource>> {
        if std::env::var_os("FIGMA_TOKEN").is_some() {
            return Ok(Some(CredentialSource::Environment));
        }
        Ok(self.keyring.load()?.map(|_| CredentialSource::Keyring))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn keyring_failure(error: keyring::Error) -> AppError {
    AppError::new(
        ErrorCode::StoreFailed,
        "The system keyring operation failed.",
    )
    .with_details(json!({"reason": error.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::{Credential, CredentialSource, KeyringBackend, PatProvider};
    use figstash_core::{AppResult, CredentialKind};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryKeyring(Mutex<Option<String>>);

    impl KeyringBackend for MemoryKeyring {
        fn load(&self) -> AppResult<Option<String>> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(|error| panic!("keyring mutex poisoned: {error}"))
                .clone())
        }

        fn store(&self, secret: &str) -> AppResult<()> {
            *self
                .0
                .lock()
                .unwrap_or_else(|error| panic!("keyring mutex poisoned: {error}")) =
                Some(secret.to_owned());
            Ok(())
        }

        fn clear(&self) -> AppResult<bool> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(|error| panic!("keyring mutex poisoned: {error}"))
                .take()
                .is_some())
        }
    }

    #[test]
    fn debug_output_redacts_the_token() {
        let token = "secret-token-that-must-not-leak";
        let credential = Credential::personal_access_token(token, CredentialSource::Environment)
            .unwrap_or_else(|error| panic!("credential creation failed: {error}"));
        let debug = format!("{credential:?}");
        assert!(!debug.contains(token));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn owned_secret_round_trips_through_keyring_without_kind_guessing() {
        let provider = PatProvider::new(MemoryKeyring::default());
        provider
            .store("synthetic-sensitive-value".to_owned())
            .unwrap_or_else(|error| panic!("credential store failed: {error}"));
        assert_eq!(
            provider
                .status()
                .unwrap_or_else(|error| panic!("credential status failed: {error}")),
            Some(CredentialSource::Keyring)
        );
        let credential = provider
            .resolve()
            .unwrap_or_else(|error| panic!("credential resolution failed: {error}"));
        assert_eq!(credential.kind(), CredentialKind::Pat);
        assert_eq!(credential.source(), CredentialSource::Keyring);
        assert!(
            provider
                .clear()
                .unwrap_or_else(|error| panic!("credential clear failed: {error}"))
        );
    }
}
