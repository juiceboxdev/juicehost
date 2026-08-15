//! OpenAPI specification for the juicehost API.

use utoipa::OpenApi;

use crate::error::ErrorResponse;
use crate::storage::StorageMetrics;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Juicehost API",
        description = "Internal file storage API for Juicebox. juicehost manages the physical storage of uploaded files, serving them with ETag-based caching, and provides health/storage metrics. Internal routes (`/internal/*`) are protected by a shared API key and are only called by juiceback.",
        version = "1.0.0",
        license(name = "GPL-3.0-or-later"),
    ),
    servers(
        (url = "/", description = "Relative to the server root"),
    ),
    paths(
        crate::handlers::index_handler,
        crate::handlers::health,
        crate::handlers::storage_handler,
        crate::handlers::config_handler,
        crate::handlers::store_file,
        crate::handlers::store_file_streaming,
        crate::handlers::store_file_ticket,
        crate::handlers::delete_file,
        crate::handlers::rename_file,
        crate::handlers::concat_files,
        crate::handlers::stat_file,
    ),
    components(schemas(
        StorageMetrics,
        ErrorResponse,
    )),
    tags(
        (name = "Files", description = "File serving endpoints"),
         (name = "Internal", description = "Internal file management endpoints. Most require the `X-Juicehost-API-Key` header; ticket uploads also accept a bearer token."),
        (name = "General", description = "Health and storage info endpoints"),
    ),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Juicehost-API-Key"))),
        );
    }
}
