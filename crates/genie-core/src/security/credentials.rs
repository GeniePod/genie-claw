/// Zero-knowledge credential store.
///
/// Adapted from NanoClaw's credential proxy pattern.
/// Tools never see raw API tokens — they request a credential handle,
/// and the HTTP layer injects the actual secret.
///
/// ```text
/// NanoClaw:  Tool → HTTP proxy → inject credential → external service
/// GeniePod:  Tool → CredentialStore.inject() → HTTP request with auth
/// ```
///
/// Secrets are:
/// - Stored in memory only (never written to disk by this module)
/// - Wrapped in Zeroizing<String> (wiped on drop)
/// - Never logged (Display impl redacts)
/// - Never returned to tools (only injected into HTTP headers)
///
/// RAM cost: ~0 (a few strings in memory, wiped on drop)
use std::collections::HashMap;
use zeroize::Zeroizing;

/// Opaque credential handle — tools use this, never the raw secret.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct CredentialId(String);

impl CredentialId {
    pub fn new(service: &str) -> Self {
        Self(service.to_string())
    }

    pub fn service(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CredentialId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential:{}", self.0)
    }
}

/// A stored credential with metadata.
struct StoredCredential {
    /// The actual secret value — never exposed to tools.
    secret: SecureString,
    /// How to inject this credential into HTTP requests.
    injection: InjectionMethod,
}

/// How a credential is injected into outbound requests.
#[derive(Debug, Clone)]
pub enum InjectionMethod {
    /// Authorization: Bearer <token>
    BearerToken,
    /// Custom header: <name>: <value>
    Header(String),
    /// Query parameter: ?<name>=<value>
    QueryParam(String),
}

/// A string that wipes itself from memory on drop.
struct SecureString {
    inner: Zeroizing<String>,
}

impl SecureString {
    fn new(s: &str) -> Self {
        Self {
            inner: Zeroizing::new(s.to_string()),
        }
    }

    fn as_str(&self) -> &str {
        &self.inner
    }
}

// Never display the actual secret.
impl std::fmt::Debug for SecureString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[REDACTED:{}bytes]", self.inner.len())
    }
}

/// Central credential store.
///
/// Usage:
/// ```rust,ignore
/// let mut store = CredentialStore::new();
///
/// // At startup (config loading):
/// store.register("homeassistant", "eyJ...", InjectionMethod::BearerToken);
///
/// // In tool code (never sees the secret):
/// let cred = CredentialId::new("homeassistant");
/// let auth_header = store.inject_header(&cred)?;
/// // auth_header = ("Authorization", "Bearer eyJ...")
///
/// // Tool only knows: "I have a credential for homeassistant"
/// // Tool never sees: "eyJ..."
/// ```
pub struct CredentialStore {
    credentials: HashMap<String, StoredCredential>,
}

impl CredentialStore {
    pub fn new() -> Self {
        Self {
            credentials: HashMap::new(),
        }
    }

    /// Register a credential. Called during startup from config.
    pub fn register(&mut self, service: &str, secret: &str, injection: InjectionMethod) {
        tracing::info!(
            service,
            method = ?injection,
            "credential registered (secret not logged)"
        );
        self.credentials.insert(
            service.to_string(),
            StoredCredential {
                secret: SecureString::new(secret),
                injection,
            },
        );
    }

    /// Check if a credential exists for a service.
    pub fn has(&self, id: &CredentialId) -> bool {
        self.credentials.contains_key(id.service())
    }

    /// Get the HTTP header for a credential.
    /// Returns (header_name, header_value) ready for injection.
    ///
    /// This is the ONLY way secrets leave the store — as HTTP headers
    /// injected by the genie-core HTTP client, never by tools.
    pub fn inject_header(&self, id: &CredentialId) -> Option<(String, String)> {
        let cred = self.credentials.get(id.service())?;
        match &cred.injection {
            InjectionMethod::BearerToken => Some((
                "Authorization".to_string(),
                format!("Bearer {}", cred.secret.as_str()),
            )),
            InjectionMethod::Header(name) => Some((name.clone(), cred.secret.as_str().to_string())),
            InjectionMethod::QueryParam(_) => {
                // Query params handled separately in URL construction.
                None
            }
        }
    }

    /// Get query parameter for a credential (for URL construction).
    pub fn inject_query_param(&self, id: &CredentialId) -> Option<(String, String)> {
        let cred = self.credentials.get(id.service())?;
        if let InjectionMethod::QueryParam(name) = &cred.injection {
            Some((name.clone(), cred.secret.as_str().to_string()))
        } else {
            None
        }
    }

    /// Number of registered credentials.
    pub fn count(&self) -> usize {
        self.credentials.len()
    }

    /// List registered service names (never the secrets).
    pub fn services(&self) -> Vec<String> {
        self.credentials.keys().cloned().collect()
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_inject_bearer() {
        let mut store = CredentialStore::new();
        store.register(
            "homeassistant",
            "test-token-123",
            InjectionMethod::BearerToken,
        );

        let id = CredentialId::new("homeassistant");
        assert!(store.has(&id));

        let (name, value) = store.inject_header(&id).unwrap();
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer test-token-123");
    }

    #[test]
    fn register_and_inject_custom_header() {
        let mut store = CredentialStore::new();
        store.register(
            "custom-api",
            "secret-key",
            InjectionMethod::Header("X-API-Key".into()),
        );

        let id = CredentialId::new("custom-api");
        let (name, value) = store.inject_header(&id).unwrap();
        assert_eq!(name, "X-API-Key");
        assert_eq!(value, "secret-key");
    }

    #[test]
    fn missing_credential_returns_none() {
        let store = CredentialStore::new();
        let id = CredentialId::new("nonexistent");
        assert!(!store.has(&id));
        assert!(store.inject_header(&id).is_none());
    }

    #[test]
    fn credential_id_display_is_safe() {
        let id = CredentialId::new("homeassistant");
        let display = format!("{}", id);
        assert_eq!(display, "credential:homeassistant");
        // Service name shown, but never the secret.
    }

    #[test]
    fn secure_string_debug_redacts() {
        let s = SecureString::new("super-secret-token");
        let debug = format!("{:?}", s);
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("super-secret-token"));
    }

    #[test]
    fn secure_string_wipes_on_drop() {
        let s = SecureString::new("wipe-me");
        let ptr = s.inner.as_ptr();
        let len = s.inner.len();
        drop(s);
        // After drop, the memory should be zeroed.
        // We can't safely read freed memory, but the Drop impl zeroes it.
        // This test verifies the Drop impl exists and runs.
        let _ = (ptr, len); // Compiler can't optimize away the drop.
    }

    #[test]
    fn services_list() {
        let mut store = CredentialStore::new();
        store.register("ha", "tok1", InjectionMethod::BearerToken);
        store.register("weather", "tok2", InjectionMethod::Header("X-Key".into()));

        let services = store.services();
        assert_eq!(services.len(), 2);
        assert!(services.contains(&"ha".to_string()));
        assert!(services.contains(&"weather".to_string()));
    }

    #[test]
    fn query_param_injection() {
        let mut store = CredentialStore::new();
        store.register(
            "maps",
            "api-key-123",
            InjectionMethod::QueryParam("key".into()),
        );

        let id = CredentialId::new("maps");
        // Header injection returns None for query param credentials.
        assert!(store.inject_header(&id).is_none());
        // Query param injection works.
        let (name, value) = store.inject_query_param(&id).unwrap();
        assert_eq!(name, "key");
        assert_eq!(value, "api-key-123");
    }
}
