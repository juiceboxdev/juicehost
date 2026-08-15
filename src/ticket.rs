//! Ticket JWT verification shared by the server middleware and upload handlers.

/// Verify a ticket JWT and return its claims on success.
pub fn verify_ticket_jwt(
    token: &str,
    secret: &str,
) -> Result<serde_json::Value, jsonwebtoken::errors::Error> {
    use jsonwebtoken::{decode, DecodingKey};

    let validation = ticket_validation();
    let token_data = decode::<serde_json::Value>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(token_data.claims)
}

/// Shared validation for ticket JWTs.
pub fn ticket_validation() -> jsonwebtoken::Validation {
    use jsonwebtoken::{Algorithm, Validation};

    const TICKET_ISS: &str = "juiceback-ticket";

    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);
    validation.set_issuer(&[TICKET_ISS]);
    validation
}
