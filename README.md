# <img src=".github/logo.png" width="48" height="48" align="top"> Juicehost

juicehost is the file storage and delivery service for [Juicebox](https://github.com/juiceboxdev/juicebox). It supports local disk and S3-compatible storage, streaming uploads, file validation, HTTP range requests, caching, and optional HTTP/3.

This repository includes `juiceutils` as a Git submodule. Clone it recursively:

```sh
git clone --recurse-submodules https://github.com/juiceboxdev/juicehost.git
cd juicehost
```

For an existing clone, run `git submodule update --init --recursive`.

## Running

Create a local environment file from `.env.example`, set `JUICEHOST_API_KEY` to pair juicehost with a juiceback instance (optional; leaving it unset disables internal-API authentication), then run:

```sh
cargo run --release
```

The local storage directory is created automatically. QUIC certificate and key handling is provided by `juiceutils`; use deployment-managed certificates and keep private keys outside the repository for production.

## Configuration

All configuration is supplied through environment variables.

| Variable | Default | Description |
|----------|---------|-------------|
| `PUBLIC_HOST` | `127.0.0.1` | HTTP bind address |
| `PUBLIC_PORT` | `6402` | HTTP port |
| `QUIC_HOST` | `PUBLIC_HOST` | HTTP/3 bind address |
| `QUIC_PORT` | `PUBLIC_PORT + 1` | HTTP/3 UDP port |
| `QUIC_CERT_PATH` | `./quic-cert.der` | HTTP/3 certificate path |
| `WORKER_THREADS` | `3` | Tokio worker threads |
| `FILES_DIR` | `./files` | Local storage directory |
| `BACKEND_URL` | none | juiceback URL for status, alias, and health requests |
| `FRONTEND_URL` | none | juicefront URL that `GET /` redirects to with a 308; unset keeps a 404 on `/` |
| `JUICEHOST_API_KEY` | empty | Shared secret for internal API requests; leave unset to disable authentication on internal endpoints |
| `TICKET_JWT_SECRET` | `JWT_SECRET`, then API key | Upload-ticket signing secret; configure a distinct value in production |
| `ALLOWED_ORIGINS` | `BACKEND_URL` | Comma-separated allowed internal request origins |
| `TRUSTED_PROXY_CIDRS` | none | Proxy CIDRs allowed to supply client IP headers |
| `MIN_FREE_SPACE_GB` | `5` | Minimum local free space before writes are rejected |
| `MAX_FILE_SIZE_MB` | `500` | Maximum file size |
| `MAX_RANGE_RESPONSE_MB` | `16` | Maximum single-range response size |
| `MAX_CONCURRENT_UPLOADS` | `16` | Concurrent storage writes |
| `MAX_CONCURRENT_DOWNLOADS` | `64` | Concurrent response streams |
| `MAX_CONCAT_PARTS` | `128` | Maximum parts in one concat request |
| `TCP_BODY_INACTIVITY_SECONDS` | `30` | Request-body inactivity timeout |
| `TCP_REQUEST_TOTAL_SECONDS` | `600` | Total TCP request timeout |
| `TCP_MAX_CONCURRENT_REQUESTS` | `512` | Concurrent TCP requests |
| `QUIC_MAX_CONNECTIONS` | `256` | Accepted HTTP/3 connections |
| `QUIC_MAX_REQUESTS` | `256` | In-flight HTTP/3 requests |
| `QUIC_HANDSHAKE_SECONDS` | `10` | HTTP/3 handshake timeout |
| `QUIC_IDLE_SECONDS` | `30` | HTTP/3 idle timeout |
| `QUIC_REQUEST_TOTAL_SECONDS` | `600` | Total HTTP/3 request timeout |
| `QUICK_LINK` | `true` | Advertise Quick Link support |
| `CUSTOM_ID` | `true` | Advertise custom file ID support |
| `DANGER_LEVEL` | `high` | Validation tier: `none`, `low`, `medium`, or `high` |
| `DEFAULT_TTL_HOURS` | `24` | Default retention period |
| `ALLOWED_TTL_HOURS` | `0.5,1,6,12,24,72,168` | Allowed retention periods |
| `IP_PEPPER` | none | Pepper for optional hashed IP bans |
| `BAN_LIST_FILE` | none | Local JSON ban-list path |
| `BAN_SYNC_URL` | none | juiceback URL used to synchronize bans |
| `BAN_SYNC_INTERVAL` | `30` | Ban synchronization interval in seconds |
| `S3_BUCKET` | none | S3 bucket; enables the S3 backend |
| `S3_REGION` | none | S3 region |
| `S3_ENDPOINT` | AWS default | S3-compatible endpoint |
| `S3_ALLOW_HTTP` | `false` | Explicitly permit a plaintext S3 endpoint for local development |
| `S3_ACCESS_KEY` | none | S3 access key |
| `S3_SECRET_KEY` | none | S3 secret key |
| `SENTRY_DSN_JUICEHOST` | none | Service-specific Sentry DSN |
| `SENTRY_DSN` | none | Fallback Sentry DSN |
| `SENTRY_ENVIRONMENT` | `production` | Sentry environment |
| `SENTRY_TRACES_SAMPLE_RATE` | `0.05` | Sentry trace sampling rate |

Numeric limits outside the ranges documented in `.env.example` fail startup. Bind public listeners and enable plaintext S3 only when the surrounding network policy is deliberate.

## API

Public endpoints:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Redirect to `FRONTEND_URL` (308) or 404 when unset |
| `GET` | `/f/*path` | Serve a file with ETag and range support |
| `GET` | `/api/health` | Service and optional juiceback health |
| `GET` | `/api/storage` | Storage capacity metrics |
| `GET` | `/api/config` | Public upload capabilities and limits |
| `GET` | `/api/openapi.json` | OpenAPI document |

Internal endpoints accept `X-Juicehost-API-Key` when the instance is paired. User-selected hosts can instead receive an `X-Juicehost-File-Capability` during upload; the same per-file capability is then required for delete, rename, and concat. `POST /internal/file/upload/:id` also accepts a signed bearer ticket, which can carry the capability without exposing it as an unsigned browser header.

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/internal/file` | Buffered multipart upload |
| `POST` | `/internal/file/stream/:id/:filename` | Streaming upload |
| `POST` | `/internal/file/upload/:id` | Ticket-based streaming upload |
| `GET` | `/internal/file/:id/stat` | Check file presence and size |
| `DELETE` | `/internal/file/:id` | Delete a file |
| `POST` | `/internal/file/:id/rename` | Rename a file ID |
| `POST` | `/internal/file/concat` | Concatenate uploaded parts |

The OpenAPI document is the machine-readable API reference.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for checks and contribution requirements. Report vulnerabilities according to [SECURITY.md](SECURITY.md).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
