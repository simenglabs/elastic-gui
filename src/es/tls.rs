//! TLS tambahan: pin fingerprint CA (PRF-06), client cert PKI dan Cloud ID (PRF-04).

use std::sync::Arc;

use base64::Engine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};

use crate::i18n::{t, tf};

/// Fingerprint SHA-256 hex, dengan atau tanpa `:` (format output setup ES 8+).
pub fn parse_fingerprint(text: &str) -> Result<Vec<u8>, String> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    if hex.len() != 64 || cleaned.len() != 64 {
        return Err(t("Fingerprint harus SHA-256: 64 digit hex (boleh dipisah ':').").into());
    }
    (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Cloud ID `nama:base64(host$es_uuid$kibana_uuid)` -> URL Elasticsearch.
pub fn decode_cloud_id(cloud_id: &str) -> Result<String, String> {
    let encoded = cloud_id
        .trim()
        .rsplit_once(':')
        .map_or(cloud_id.trim(), |(_, b)| b);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| t("Cloud ID tidak valid (bukan base64)."))?;
    let text = String::from_utf8(bytes).map_err(|_| t("Cloud ID tidak valid (bukan UTF-8)."))?;
    let mut parts = text.split('$');
    let host = parts
        .next()
        .filter(|h| !h.is_empty())
        .ok_or(t("Cloud ID tanpa host."))?;
    let es = parts
        .next()
        .filter(|h| !h.is_empty())
        .ok_or(t("Cloud ID tanpa UUID Elasticsearch."))?;
    let (host, port) = host.split_once(':').unwrap_or((host, "443"));
    Ok(format!("https://{es}.{host}:{port}"))
}

/// Terima sertifikat server jika ia atau salah satu sertifikat di rantainya memiliki
/// fingerprint yang dipin. Tanda tangan handshake tetap diverifikasi.
#[derive(Debug)]
struct PinVerifier {
    fingerprint: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let matches = std::iter::once(end_entity).chain(intermediates).any(|c| {
            ::ring::digest::digest(&::ring::digest::SHA256, c.as_ref()).as_ref() == self.fingerprint
        });
        if matches {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                t("fingerprint sertifikat server tidak cocok dengan yang dipin di profile").into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Konfigurasi rustls dengan pin fingerprint, plus client cert (PEM cert+key) bila ada.
pub fn pinned_config(
    fingerprint: &str,
    identity_pem: Option<&[u8]>,
) -> Result<ClientConfig, String> {
    let provider = Arc::new(ring::default_provider());
    let verifier = PinVerifier {
        fingerprint: parse_fingerprint(fingerprint)?,
        provider: provider.clone(),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let mut config = match identity_pem {
        None => builder.with_no_client_auth(),
        Some(pem) => {
            let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
                .collect::<Result<_, _>>()
                .map_err(|e| tf("Client cert tidak valid: {0}", &[&e]))?;
            let key = PrivateKeyDer::from_pem_slice(pem)
                .map_err(|e| tf("Private key tidak valid: {0}", &[&e]))?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| tf("Client cert ditolak: {0}", &[&e]))?
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    (
        "Fingerprint harus SHA-256: 64 digit hex (boleh dipisah ':').",
        "The fingerprint must be SHA-256: 64 hex digits (':' separators allowed).",
    ),
    (
        "Cloud ID tidak valid (bukan base64).",
        "Invalid Cloud ID (not base64).",
    ),
    (
        "Cloud ID tidak valid (bukan UTF-8).",
        "Invalid Cloud ID (not UTF-8).",
    ),
    ("Cloud ID tanpa host.", "Cloud ID has no host."),
    (
        "Cloud ID tanpa UUID Elasticsearch.",
        "Cloud ID has no Elasticsearch UUID.",
    ),
    (
        "fingerprint sertifikat server tidak cocok dengan yang dipin di profile",
        "the server certificate fingerprint does not match the one pinned in the profile",
    ),
    ("Client cert tidak valid: {0}", "Invalid client cert: {0}"),
    ("Private key tidak valid: {0}", "Invalid private key: {0}"),
    ("Client cert ditolak: {0}", "Client cert rejected: {0}"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("tls.rs"), EN);
    }

    #[test]
    fn parses_fingerprints() {
        let hex = "AB".repeat(32);
        let colon = vec!["ab"; 32].join(":");
        assert_eq!(parse_fingerprint(&hex).unwrap(), vec![0xab; 32]);
        assert_eq!(parse_fingerprint(&colon).unwrap(), vec![0xab; 32]);
        assert!(parse_fingerprint("abcd").is_err());
        assert!(parse_fingerprint(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn decodes_cloud_id() {
        let raw = base64::engine::general_purpose::STANDARD
            .encode("us-east-1.aws.found.io$abc123$kib456");
        assert_eq!(
            decode_cloud_id(&format!("my-deploy:{raw}")).unwrap(),
            "https://abc123.us-east-1.aws.found.io:443"
        );
        let with_port = base64::engine::general_purpose::STANDARD.encode("host.io:9243$es1$kb1");
        assert_eq!(
            decode_cloud_id(&with_port).unwrap(),
            "https://es1.host.io:9243"
        );
        assert!(decode_cloud_id("bukan:base64!!").is_err());
    }

    #[test]
    fn builds_pinned_config() {
        assert!(pinned_config(&"ab".repeat(32), None).is_ok());
        assert!(pinned_config("salah", None).is_err());
    }
}
