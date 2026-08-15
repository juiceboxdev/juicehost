# Security Policy

## Reporting a Vulnerability

To report a security issue, contact Juiceydev directly:

- Discord: `juiceydev`
- Email: me@juicey.dev

Do not open a public issue for suspected vulnerabilities. Include affected versions, reproduction steps, impact, and any proposed remediation.

Reports will be acknowledged as soon as practical. Confirmed issues will be fixed on the supported release line and disclosed after a patched release is available.

## Deployment

Treat `JUICEHOST_API_KEY`, `TICKET_JWT_SECRET`, `IP_PEPPER`, S3 credentials, QUIC private keys, file names, and file identifiers as sensitive. Keep secrets outside the repository, use TLS for public and S3 traffic, restrict internal routes at the network boundary, and review Sentry settings before enabling telemetry.
