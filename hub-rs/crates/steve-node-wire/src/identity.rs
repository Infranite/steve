//! A Node's Ed25519 identity: generated at pairing, kept in one file only its owner may read. The
//! Hub stores the public key; a pairing code is stored only as its sha256.

use std::io;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

pub struct Identity {
    key: SigningKey,
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// What the confirm page and the CLI both show: the first 16 hex digits of sha256(pubkey).
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    hex::encode(sha256(pubkey))[..16].to_string()
}

pub fn verify(pubkey: &[u8; 32], msg: &[u8], sig: &[u8]) -> bool {
    let Ok(k) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(sig) else {
        return false;
    };
    k.verify_strict(msg, &sig).is_ok()
}

/// What a pairing claim signs: proof the claimant holds the key it registers, for this code.
pub fn pair_message(code: &str, pubkey: &[u8; 32]) -> Vec<u8> {
    let mut m = b"steve-pair-v1".to_vec();
    m.extend_from_slice(&sha256(code.as_bytes()));
    m.extend_from_slice(pubkey);
    m
}

pub fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(b)
}

impl Identity {
    pub fn generate() -> io::Result<Identity> {
        Ok(Identity::from_seed(random::<32>()?))
    }

    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity {
            key: SigningKey::from_bytes(&seed),
        }
    }

    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.key.sign(msg).to_bytes().to_vec()
    }

    /// Refuses a key file any other user could read: a copied key is a copied Node.
    pub fn load(path: &Path) -> io::Result<Identity> {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is readable by others (mode {:o}); chmod 600 it",
                    path.display(),
                    mode & 0o777
                ),
            ));
        }
        let text = std::fs::read_to_string(path)?;
        let seed: [u8; 32] = hex::decode(text.trim())
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad identity key"))?;
        Ok(Identity::from_seed(seed))
    }

    /// Written once, 0600 from the start (never widened then narrowed).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(hex::encode(self.key.to_bytes()).as_bytes())?;
        f.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("stv-id-{}", hex::encode(random::<6>().unwrap())));
        let p = dir.join("identity.key");
        let id = Identity::generate().unwrap();
        id.save(&p).unwrap();
        assert_eq!(Identity::load(&p).unwrap().public(), id.public());
        assert!(id.save(&p).is_err(), "never overwritten");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = Identity::load(&p).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let msg = pair_message("stv1-x", &id.public());
        assert!(verify(&id.public(), &msg, &id.sign(&msg)));
        assert!(!verify(&id.public(), b"other", &id.sign(&msg)));
        assert_eq!(id.fingerprint().len(), 16);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
