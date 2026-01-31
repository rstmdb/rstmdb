//! TLS configuration and connector for client.

use crate::connection::TlsClientConfig;
use crate::error::ClientError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::RootCertStore;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::TlsConnector;

/// Creates a TLS connector from client configuration.
pub fn create_tls_connector(
    config: &TlsClientConfig,
    server_host: &str,
) -> Result<(TlsConnector, ServerName<'static>), ClientError> {
    // Build root cert store
    let root_store = if let Some(ref ca_path) = config.ca_cert_path {
        let certs = load_certs(ca_path)?;
        let mut store = RootCertStore::empty();
        for cert in certs {
            store
                .add(cert)
                .map_err(|e| ClientError::TlsConfig(format!("invalid CA cert: {}", e)))?;
        }
        store
    } else {
        // Use system roots
        let mut store = RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        store
    };

    // Build client config
    let builder = rustls::ClientConfig::builder().with_root_certificates(root_store);

    let client_config = if let (Some(cert_path), Some(key_path)) =
        (&config.client_cert_path, &config.client_key_path)
    {
        // mTLS: provide client certificate
        let certs = load_certs(cert_path)?;
        let key = load_private_key(key_path)?;

        builder
            .with_client_auth_cert(certs, key)
            .map_err(|e| ClientError::TlsConfig(format!("invalid client cert/key: {}", e)))?
    } else {
        builder.with_no_client_auth()
    };

    let connector = TlsConnector::from(Arc::new(client_config));

    // Determine server name for SNI
    let server_name_str = config.server_name.as_deref().unwrap_or(server_host);

    let server_name = ServerName::try_from(server_name_str.to_string())
        .map_err(|_| ClientError::TlsConfig(format!("invalid server name: {}", server_name_str)))?;

    Ok((connector, server_name))
}

/// Creates an insecure TLS connector that skips certificate verification.
/// WARNING: Only use for development/testing.
pub fn create_insecure_tls_connector(
    config: &TlsClientConfig,
    server_host: &str,
) -> Result<(TlsConnector, ServerName<'static>), ClientError> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::UnixTime;
    use rustls::DigitallySignedStruct;

    #[derive(Debug)]
    struct InsecureVerifier;

    impl ServerCertVerifier for InsecureVerifier {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA384,
                rustls::SignatureScheme::RSA_PKCS1_SHA512,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PSS_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA512,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(client_config));

    let server_name_str = config.server_name.as_deref().unwrap_or(server_host);
    let server_name = ServerName::try_from(server_name_str.to_string())
        .map_err(|_| ClientError::TlsConfig(format!("invalid server name: {}", server_name_str)))?;

    Ok((connector, server_name))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, ClientError> {
    let file = File::open(path)
        .map_err(|e| ClientError::TlsConfig(format!("cannot open cert file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);

    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ClientError::TlsConfig(format!("invalid cert file {:?}: {}", path, e)))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, ClientError> {
    let file = File::open(path)
        .map_err(|e| ClientError::TlsConfig(format!("cannot open key file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| ClientError::TlsConfig(format!("invalid key file {:?}: {}", path, e)))?
        {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Sec1Key(key)) => return Ok(key.into()),
            None => {
                return Err(ClientError::TlsConfig(format!(
                    "no private key found in {:?}",
                    path
                )))
            }
            _ => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_invalid_cert_path() {
        let result = load_certs(Path::new("/nonexistent/cert.pem"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot open"));
    }

    #[test]
    fn test_load_invalid_key_path() {
        let result = load_private_key(Path::new("/nonexistent/key.pem"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot open"));
    }
}
