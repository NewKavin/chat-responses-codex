# Upstream Custom CA Trust Design

**Date:** 2026-07-30

## Problem

An upstream account can discover models successfully through an HTTP IP and port, but discovery fails after the same service moves to an HTTPS internal domain. CC-Switch succeeds with the same URL and key because it runs with the host trust store, while this gateway uses Reqwest with Rustls WebPKI public roots inside a container. The gateway has no supported way to add private CA certificates.

The discovery UI also hides the safe per-key failure returned by the backend when every key fails, leaving only the generic message that all keys failed.

## Goals

- Keep the existing WebPKI public roots and append operator-provided private CA certificates.
- Accept either one PEM bundle file or a directory containing multiple PEM certificate files.
- Apply the same trust configuration to inference, model discovery, model probes, batch account creation, and other upstream HTTP calls.
- Fail startup clearly when an explicitly configured CA source cannot be read or parsed.
- Show safe per-key discovery errors without exposing URLs, keys, certificates, or provider response bodies.
- Provide a repository-local `certs/` mount point while keeping environment-specific certificates out of Git.

## Non-Goals

- Disabling hostname or certificate verification.
- Loading private keys or configuring mutual TLS.
- Automatically watching certificate files for changes. A gateway restart activates updates.
- Copying or mounting the host's complete `/etc/ssl/certs` tree over the container trust directory.

## Configuration

Add one optional environment variable:

```env
UPSTREAM_CA_CERT_PATH=/certs
```

The path may identify:

1. A PEM file containing one or more certificates.
2. A directory containing `.crt` and `.pem` files.

When the variable is absent or blank, behavior remains unchanged and only the existing public roots are used.

For a directory, the gateway sorts entries by file name, ignores other extensions, and parses every selected file as a PEM bundle. The configured source must yield at least one certificate. An unreadable path, empty selected set, or invalid selected file is a startup error that names the path but never prints certificate contents.

## Repository And Deployment Layout

Add a repository-local directory:

```text
certs/
├── README.md
├── root-ca.crt
├── intermediate-ca.crt
└── another-internal-ca.pem
```

Only `README.md` and ignore rules are committed. Environment-specific `.crt` and `.pem` files remain untracked.

Docker Compose mounts the directory read-only:

```yaml
services:
  chat-responses-codex:
    environment:
      UPSTREAM_CA_CERT_PATH: ${UPSTREAM_CA_CERT_PATH:-}
    volumes:
      - ./certs:/certs:ro
```

An internal deployment sets `UPSTREAM_CA_CERT_PATH=/certs`. A deployment that already maintains a host bundle may instead mount that single file into `/certs/host-ca-bundle.pem` and point the variable at it.

Only CA certificates belong in this directory. Server private keys must not be mounted into the gateway.

## Runtime Design

Configuration loading resolves and validates the optional CA source before the application begins serving traffic. Parsed `reqwest::Certificate` values are added to the existing Reqwest client builder with `add_root_certificate`; default public roots remain enabled.

Both the normal client and the no-proxy direct client are built from the same validated certificate set. The no-proxy distinction changes routing only, not trust.

Administrative upstream operations must stop constructing independent default clients. Model discovery, model qualification/probing, and batch account discovery reuse the application upstream clients selected for the target URL, while retaining their request-level admin timeout. This prevents inference and administrative checks from having different TLS behavior.

No runtime fallback silently drops configured private roots. If client construction fails after a CA path is configured, startup fails rather than continuing with public roots only.

## Error Handling

The backend continues to sanitize provider failures. It may report bounded categories such as connection failure, timeout, HTTP status, invalid JSON, missing model data, or invalid configured CA. It must not return the provider body or request credentials.

When all keys fail, the frontend appends each indexed safe error to the generic message instead of allowing the generic message to replace those details. This makes TLS/network failures distinguishable from HTTP and response-format failures.

## Security

- TLS hostname and chain verification stay enabled.
- Public roots stay enabled; custom roots are additive.
- Only `.crt` and `.pem` files are loaded from a configured directory.
- Certificate contents and upstream credentials are never logged or returned.
- The container mount is read-only.
- Environment-specific certificates are ignored by Git.

## Testing

Backend tests cover:

- no configured path preserves current client construction;
- a valid single PEM bundle loads all certificates;
- a directory loads sorted `.crt` and `.pem` bundles and ignores unrelated files;
- missing, empty, unreadable, or invalid configured sources fail clearly;
- both normal and direct upstream clients receive the same custom roots;
- administrative discovery uses the application client and succeeds against a TLS test server signed by a configured private CA;
- the same TLS server fails without that CA.

Frontend tests cover an all-key failure response and assert that the rendered notification includes both the generic summary and indexed safe errors.

Deployment tests cover the optional Compose environment variable, the read-only `certs/` mount, and repository ignore rules.

## Rollout

Existing deployments need no configuration change. Internal deployments place their CA files in `certs/`, set `UPSTREAM_CA_CERT_PATH=/certs`, rebuild or restart the gateway, and retry model discovery. Updating certificates requires replacing the mounted files and restarting the gateway.
