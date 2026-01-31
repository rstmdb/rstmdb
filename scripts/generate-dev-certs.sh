#!/bin/bash
# Generate self-signed certificates for development
#
# Usage: ./scripts/generate-dev-certs.sh [output-dir]
# Default output directory: ./dev-certs

set -e

CERT_DIR="${1:-./dev-certs}"
mkdir -p "$CERT_DIR"

echo "Generating development certificates in $CERT_DIR/"
echo ""

# Generate CA private key and certificate
echo "1. Generating CA certificate..."
openssl req -x509 -newkey rsa:4096 -days 365 -nodes \
    -keyout "$CERT_DIR/ca-key.pem" \
    -out "$CERT_DIR/ca-cert.pem" \
    -subj "/CN=rstmdb-dev-ca/O=rstmdb-dev"

# Generate server private key and CSR
echo "2. Generating server certificate..."
openssl req -newkey rsa:4096 -nodes \
    -keyout "$CERT_DIR/server-key.pem" \
    -out "$CERT_DIR/server-req.pem" \
    -subj "/CN=localhost/O=rstmdb-dev"

# Create server certificate signed by CA
openssl x509 -req -days 365 \
    -in "$CERT_DIR/server-req.pem" \
    -CA "$CERT_DIR/ca-cert.pem" \
    -CAkey "$CERT_DIR/ca-key.pem" \
    -CAcreateserial \
    -out "$CERT_DIR/server-cert.pem" \
    -extfile <(echo "subjectAltName=DNS:localhost,IP:127.0.0.1")

# Generate client certificate for mTLS (optional)
echo "3. Generating client certificate (for mTLS)..."
openssl req -newkey rsa:4096 -nodes \
    -keyout "$CERT_DIR/client-key.pem" \
    -out "$CERT_DIR/client-req.pem" \
    -subj "/CN=rstmdb-client/O=rstmdb-dev"

openssl x509 -req -days 365 \
    -in "$CERT_DIR/client-req.pem" \
    -CA "$CERT_DIR/ca-cert.pem" \
    -CAkey "$CERT_DIR/ca-key.pem" \
    -CAcreateserial \
    -out "$CERT_DIR/client-cert.pem"

# Cleanup CSR files
rm -f "$CERT_DIR"/*.srl "$CERT_DIR"/*-req.pem

echo ""
echo "Certificates generated successfully!"
echo ""
echo "Files created:"
echo "  $CERT_DIR/ca-cert.pem      - CA certificate (for client verification)"
echo "  $CERT_DIR/ca-key.pem       - CA private key"
echo "  $CERT_DIR/server-cert.pem  - Server certificate"
echo "  $CERT_DIR/server-key.pem   - Server private key"
echo "  $CERT_DIR/client-cert.pem  - Client certificate (for mTLS)"
echo "  $CERT_DIR/client-key.pem   - Client private key (for mTLS)"
echo ""
echo "Server configuration (config.yaml):"
echo "  tls:"
echo "    enabled: true"
echo "    cert_path: \"$CERT_DIR/server-cert.pem\""
echo "    key_path: \"$CERT_DIR/server-key.pem\""
echo "    # For mTLS:"
echo "    # require_client_cert: true"
echo "    # client_ca_path: \"$CERT_DIR/ca-cert.pem\""
echo ""
echo "Client usage:"
echo "  # Standard TLS:"
echo "  rstmdb-cli --tls --ca-cert $CERT_DIR/ca-cert.pem ..."
echo ""
echo "  # Insecure (skip verification):"
echo "  rstmdb-cli --tls --insecure ..."
echo ""
echo "  # mTLS:"
echo "  rstmdb-cli --tls --ca-cert $CERT_DIR/ca-cert.pem \\"
echo "             --client-cert $CERT_DIR/client-cert.pem \\"
echo "             --client-key $CERT_DIR/client-key.pem ..."
