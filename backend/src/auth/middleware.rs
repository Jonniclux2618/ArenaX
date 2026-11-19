use crate::auth::jwt_service::{Claims, JwtError, JwtService, TokenType};
use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    error::{ErrorForbidden, ErrorUnauthorized},
    http::Method,
    Error, HttpMessage,
};
use futures::future::LocalBoxFuture;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::future::{ready, Ready};
use std::rc::Rc;
use tracing::{debug, warn};

fn is_public_route(req: &ServiceRequest) -> bool {
    let path = req.path();
    let method = req.method();
    if method == Method::OPTIONS {
        return true;
    }
    if matches!(
        (method, path),
        (&Method::GET, "/api/health")
            | (&Method::GET, "/api/health/live")
            | (&Method::GET, "/api/health/ready")
            | (&Method::GET, "/api/csrf-token")
            | (&Method::POST, "/api/auth/register")
            | (&Method::POST, "/api/auth/login")
            | (&Method::POST, "/api/auth/refresh")
            | (&Method::POST, "/api/auth/otp/send")
            | (&Method::POST, "/api/auth/otp/verify")
            | (&Method::GET, "/api/tournaments")
            | (&Method::GET, "/metrics")
    ) {
        return true;
    }

    (method == Method::GET
        && (path.starts_with("/api/docs")
            || path.starts_with("/api/leaderboards/")
            || (path.starts_with("/api/tournaments/")
                && path["/api/tournaments/".len()..].matches('/').count() == 0)))
        || (method == Method::POST && path.starts_with("/api/webhooks/"))
}

#[derive(Debug, Deserialize)]
struct AuthorizationMatrix {
    operations: Vec<AuthorizationOperation>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationOperation {
    backend: BackendPolicy,
}

#[derive(Debug, Deserialize)]
struct BackendPolicy {
    path_prefix: String,
    roles: Vec<String>,
}

static AUTHORIZATION_MATRIX: Lazy<AuthorizationMatrix> = Lazy::new(|| {
    serde_json::from_str(include_str!("../../../security/authorization-policy.json"))
        .expect("shared authorization policy must be valid JSON")
});

fn required_roles(path: &str) -> Option<&'static [String]> {
    AUTHORIZATION_MATRIX
        .operations
        .iter()
        .find(|operation| path.starts_with(&operation.backend.path_prefix))
        .map(|operation| operation.backend.roles.as_slice())
}

fn has_required_role(principal_roles: &[String], required_roles: &[String]) -> bool {
    principal_roles.iter().any(|role| {
        required_roles
            .iter()
            .any(|required| role.eq_ignore_ascii_case(required))
    })
}

/// Authentication middleware for protecting routes
pub struct AuthMiddleware {
    jwt_service: Rc<JwtService>,
}

impl AuthMiddleware {
    pub fn new(jwt_service: JwtService) -> Self {
        Self {
            jwt_service: Rc::new(jwt_service),
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddlewareService {
            service: Rc::new(service),
            jwt_service: self.jwt_service.clone(),
        }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: Rc<S>,
    jwt_service: Rc<JwtService>,
}

impl<S, B> Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let jwt_service = self.jwt_service.clone();
        let service = self.service.clone();

        Box::pin(async move {
            // ── Token extraction ─────────────────────────────────────────────
            // Priority order:
            //   1. Authorization: Bearer <token>  (API clients, mobile)
            //   2. auth_token httpOnly cookie      (browser SPA)
            let token_opt: Option<String> = req
                .headers()
                .get("Authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned))
                .or_else(|| {
                    req.cookie(crate::http::auth_handler::ACCESS_TOKEN_COOKIE)
                        .map(|c| c.value().to_owned())
                });

            let token = match token_opt {
                Some(t) => t,
                None => {
                    if is_public_route(&req) {
                        return service.call(req).await;
                    }
                    warn!("Missing authorization header and auth cookie");
                    return Err(ErrorUnauthorized("Authentication required"));
                }
            };

            // ── Token validation ─────────────────────────────────────────────
            match jwt_service.validate_token(&token).await {
                Ok(claims) => {
                    if claims.token_type != TokenType::Access {
                        return Err(ErrorUnauthorized("Access token required"));
                    }
                    if let Some(allowed_roles) = required_roles(req.path()) {
                        if !has_required_role(&claims.roles, allowed_roles) {
                            return Err(ErrorForbidden("Insufficient permissions"));
                        }
                    }
                    debug!(user_id = %claims.sub, "Request authenticated");
                    req.extensions_mut().insert(claims);
                    service.call(req).await
                }
                Err(JwtError::TokenExpired) => {
                    warn!("Token expired");
                    Err(ErrorUnauthorized("Token expired"))
                }
                Err(JwtError::TokenBlacklisted) => {
                    warn!("Token blacklisted");
                    Err(ErrorForbidden("Token has been revoked"))
                }
                Err(JwtError::SessionNotFound) => {
                    warn!("Session not found");
                    Err(ErrorUnauthorized("Session expired or invalid"))
                }
                Err(e) => {
                    warn!(error = %e, "Token validation failed");
                    Err(ErrorUnauthorized(format!("Invalid token: {}", e)))
                }
            }
        })
    }
}

/// Extract claims from request (use in route handlers)
pub trait ClaimsExt {
    fn claims(&self) -> Option<Claims>;
    fn user_id(&self) -> Option<uuid::Uuid>;
}

impl ClaimsExt for actix_web::HttpRequest {
    fn claims(&self) -> Option<Claims> {
        let claims = self.extensions().get::<Claims>().cloned();
        // Attach the caller to the request's tracing span (#1084) so slow
        // database queries logged during this request carry `user_id` too.
        if let Some(ref c) = claims {
            tracing::Span::current().record("user_id", tracing::field::display(&c.sub));
        }
        claims
    }

    fn user_id(&self) -> Option<uuid::Uuid> {
        self.claims()
            .and_then(|c| uuid::Uuid::parse_str(&c.sub).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_authorization_matrix_allows_and_denies_expected_principals() {
        assert!(!AUTHORIZATION_MATRIX.operations.is_empty());
        for operation in &AUTHORIZATION_MATRIX.operations {
            let required = &operation.backend.roles;
            assert!(has_required_role(required, required));
            assert!(!has_required_role(&["player".to_string()], required));
            assert!(required_roles(&operation.backend.path_prefix).is_some());
        }
    }

    #[test]
    fn test_claims_ext_interface() {
        // This test just ensures the trait compiles
        assert!(true);
    }
}
