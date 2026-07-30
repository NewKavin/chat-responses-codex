# Internal CA Certificates

Place environment-specific internal CA certificates in this directory and set:

```env
UPSTREAM_CA_CERT_PATH=/certs
```

The gateway loads regular `.crt` and `.pem` files in file-name order. Each file may contain one or more PEM certificates. Public WebPKI roots remain enabled and these certificates are added to them.

Only CA certificates belong here. Do not place server private keys in this directory. Certificate files are intentionally ignored by Git. Restart the gateway after changing them.
