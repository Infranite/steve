//! Types that hold secret bytes. None of them implements `Clone`, `Display` or `Serialize`; `Debug`
//! prints only the type name, so a secret can't slip into a log line or an error through `{:?}`.
//! All of them are zeroed on drop.

use std::fmt;

use zeroize::Zeroizing;

/// A 256-bit key. `Kek` and `Dek` are distinct types so a data key can never be passed where the
/// workspace master key is expected (and the other way round).
macro_rules! key_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        pub struct $name(Zeroizing<[u8; 32]>);

        impl $name {
            /// A fresh random key from the OS generator.
            pub fn generate() -> Result<Self, super::crypto::CryptoError> {
                let mut k = Zeroizing::new([0u8; 32]);
                getrandom::fill(k.as_mut()).map_err(|_| super::crypto::CryptoError::Random)?;
                Ok(Self(k))
            }

            /// Copies a 32-byte slice; anything else is `None`.
            pub(crate) fn from_slice(s: &[u8]) -> Option<Self> {
                let mut k = Zeroizing::new([0u8; 32]);
                if s.len() != 32 {
                    return None;
                }
                k.copy_from_slice(s);
                Some(Self(k))
            }

            pub(crate) fn bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(…)"))
            }
        }
    };
}

key_type!(
    /// The workspace master key (key-encryption key). Lives in the key store; the process caches it.
    Kek
);
key_type!(
    /// The per-item data key. Stored only wrapped by the KEK (`vault_items.dek_wrapped`).
    Dek
);

/// Decrypted secret bytes (a field value). Read with [`SecretBytes::expose`] at the one place that
/// needs the plaintext.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(v: Vec<u8>) -> Self {
        Self(Zeroizing::new(v))
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(…)")
    }
}

/// The one-time recovery code: the KEK plus a checksum in base32, eight groups of seven characters.
/// Shown once (`POST /vault/init`, `/vault/rotate`), never stored.
pub struct RecoveryCode(Zeroizing<String>);

impl RecoveryCode {
    pub(crate) fn new(s: Zeroizing<String>) -> Self {
        Self(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryCode(…)")
    }
}
