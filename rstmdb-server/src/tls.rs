//! TLS configuration and acceptor.

use crate::config::TlsConfig;
use crate::error::ServerError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Loads TLS certificates and creates a TLS acceptor.
pub fn create_tls_acceptor(config: &TlsConfig) -> Result<TlsAcceptor, ServerError> {
    // Validate configuration before loading files
    let cert_path = config
        .cert_path
        .as_ref()
        .ok_or_else(|| ServerError::TlsConfig("cert_path not set".into()))?;
    let key_path = config
        .key_path
        .as_ref()
        .ok_or_else(|| ServerError::TlsConfig("key_path not set".into()))?;

    // Check mTLS config early
    if config.require_client_cert && config.client_ca_path.is_none() {
        return Err(ServerError::TlsConfig(
            "client_ca_path not set for mTLS".into(),
        ));
    }

    // Load server certificate chain
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    // Build server config
    let server_config = if config.require_client_cert {
        // mTLS: require and verify client certificates
        let client_ca_path = config.client_ca_path.as_ref().expect("already validated");

        let client_certs = load_certs(client_ca_path)?;
        let mut root_store = RootCertStore::empty();
        for cert in client_certs {
            root_store
                .add(cert)
                .map_err(|e| ServerError::TlsConfig(format!("invalid client CA cert: {}", e)))?;
        }

        let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .map_err(|e| {
                ServerError::TlsConfig(format!("failed to build client verifier: {}", e))
            })?;

        rustls::ServerConfig::builder()
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(certs, key)
            .map_err(|e| ServerError::TlsConfig(format!("invalid server cert/key: {}", e)))?
    } else {
        // Standard TLS: no client cert required
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| ServerError::TlsConfig(format!("invalid server cert/key: {}", e)))?
    };

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, ServerError> {
    let file = File::open(path)
        .map_err(|e| ServerError::TlsConfig(format!("cannot open cert file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);

    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ServerError::TlsConfig(format!("invalid cert file {:?}: {}", path, e)))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, ServerError> {
    let file = File::open(path)
        .map_err(|e| ServerError::TlsConfig(format!("cannot open key file {:?}: {}", path, e)))?;
    let mut reader = BufReader::new(file);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| ServerError::TlsConfig(format!("invalid key file {:?}: {}", path, e)))?
        {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => return Ok(key.into()),
            Some(rustls_pemfile::Item::Sec1Key(key)) => return Ok(key.into()),
            None => {
                return Err(ServerError::TlsConfig(format!(
                    "no private key found in {:?}",
                    path
                )))
            }
            _ => continue, // Skip other PEM items (certs, etc.)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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

    #[test]
    fn test_load_empty_key_file() {
        let mut key_file = NamedTempFile::new().unwrap();
        key_file.write_all(b"not a valid key").unwrap();

        let result = load_private_key(key_file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no private key"));
    }

    #[test]
    fn test_create_acceptor_missing_cert() {
        let config = TlsConfig {
            enabled: true,
            cert_path: None,
            key_path: Some("/some/key.pem".into()),
            require_client_cert: false,
            client_ca_path: None,
        };

        let result = create_tls_acceptor(&config);
        match result {
            Err(e) => assert!(e.to_string().contains("cert_path not set")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn test_create_acceptor_missing_key() {
        let config = TlsConfig {
            enabled: true,
            cert_path: Some("/some/cert.pem".into()),
            key_path: None,
            require_client_cert: false,
            client_ca_path: None,
        };

        let result = create_tls_acceptor(&config);
        match result {
            Err(e) => assert!(e.to_string().contains("key_path not set")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn test_create_acceptor_mtls_missing_ca() {
        // Note: With require_client_cert=true and cert/key paths set,
        // the function first checks for client_ca_path before loading certs.
        // But since cert_path doesn't exist, it will fail at loading first.
        // This test just verifies that an error is returned.
        let config = TlsConfig {
            enabled: true,
            cert_path: Some("/nonexistent/cert.pem".into()),
            key_path: Some("/nonexistent/key.pem".into()),
            require_client_cert: true,
            client_ca_path: None,
        };

        let result = create_tls_acceptor(&config);
        // Should fail at client_ca_path check (before trying to load certs)
        match result {
            Err(e) => assert!(e.to_string().contains("client_ca_path not set")),
            Ok(_) => panic!("expected error"),
        }
    }
}
