use age_core::{
    format::{FileKey, Stanza},
    primitives::{aead_encrypt, hkdf},
    secrecy::ExposeSecret,
};
use p256::{ecdh::EphemeralSecret, elliptic_curve::sec1::ToEncodedPoint};
use rand::rngs::OsRng;
use std::convert::TryInto;

use crate::{p256::Recipient, STANZA_TAG};

pub(crate) const STANZA_KEY_LABEL: &[u8] = b"piv-p256";

const TAG_BYTES: usize = 4;
const EPK_BYTES: usize = 33;
const ENCRYPTED_FILE_KEY_BYTES: usize = 32;

/// The ephemeral key bytes in a piv-p256 stanza.
///
/// The bytes contain a compressed SEC-1 encoding of a valid point.
#[derive(Debug)]
pub(crate) struct EphemeralKeyBytes(p256::EncodedPoint);

impl EphemeralKeyBytes {
    fn from_bytes(bytes: [u8; EPK_BYTES]) -> Option<Self> {
        let encoded = p256::EncodedPoint::from_bytes(&bytes).ok()?;
        if encoded.is_compressed() && encoded.decompress().is_some() {
            Some(EphemeralKeyBytes(encoded))
        } else {
            None
        }
    }

    fn from_public_key(epk: &p256::PublicKey) -> Self {
        EphemeralKeyBytes(epk.to_encoded_point(true))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub(crate) fn decompress(&self) -> p256::EncodedPoint {
        self.0
            .decompress()
            .expect("EphemeralKeyBytes is a valid compressed encoding by construction")
    }
}

#[derive(Debug)]
pub(crate) struct RecipientLine {
    pub(crate) tag: [u8; TAG_BYTES],
    pub(crate) epk_bytes: EphemeralKeyBytes,
    pub(crate) encrypted_file_key: [u8; ENCRYPTED_FILE_KEY_BYTES],
}

impl From<RecipientLine> for Stanza {
    fn from(r: RecipientLine) -> Self {
        Stanza {
            tag: STANZA_TAG.to_owned(),
            args: vec![
                base64::encode_config(&r.tag, base64::STANDARD_NO_PAD),
                base64::encode_config(r.epk_bytes.as_bytes(), base64::STANDARD_NO_PAD),
            ],
            body: r.encrypted_file_key.to_vec(),
        }
    }
}

impl RecipientLine {
    pub(super) fn from_stanza(s: &Stanza) -> Option<Result<Self, ()>> {
        if s.tag != STANZA_TAG {
            return None;
        }

        fn base64_arg<A: AsRef<[u8]>, B: AsMut<[u8]>>(arg: &A, mut buf: B) -> Option<B> {
            if arg.as_ref().len() != ((4 * buf.as_mut().len()) + 2) / 3 {
                return None;
            }

            base64::decode_config_slice(arg, base64::STANDARD_NO_PAD, buf.as_mut())
                .ok()
                .map(|_| buf)
        }

        let (tag, epk_bytes) = match &s.args[..] {
            [tag, epk_bytes] => (
                base64_arg(tag, [0; TAG_BYTES]),
                base64_arg(epk_bytes, [0; EPK_BYTES]).and_then(EphemeralKeyBytes::from_bytes),
            ),
            _ => (None, None),
        };

        Some(match (tag, epk_bytes, s.body[..].try_into()) {
            (Some(tag), Some(epk_bytes), Ok(encrypted_file_key)) => Ok(RecipientLine {
                tag,
                epk_bytes,
                encrypted_file_key,
            }),
            // Anything else indicates a structurally-invalid stanza.
            _ => Err(()),
        })
    }

    pub(crate) fn wrap_file_key(file_key: &FileKey, pk: &Recipient) -> Self {
        let esk = EphemeralSecret::random(OsRng);
        let epk = esk.public_key();
        let epk_bytes = EphemeralKeyBytes::from_public_key(&epk);

        let shared_secret = esk.diffie_hellman(pk.public_key());

        let mut salt = vec![];
        salt.extend_from_slice(epk_bytes.as_bytes());
        salt.extend_from_slice(pk.to_encoded().as_bytes());

        let enc_key = hkdf(&salt, STANZA_KEY_LABEL, shared_secret.as_bytes());

        let encrypted_file_key = {
            let mut key = [0; ENCRYPTED_FILE_KEY_BYTES];
            key.copy_from_slice(&aead_encrypt(&enc_key, file_key.expose_secret()));
            key
        };

        RecipientLine {
            tag: pk.tag(),
            epk_bytes,
            encrypted_file_key,
        }
    }
}
