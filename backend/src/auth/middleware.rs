use crate::auth::jwt_service::{Claims, JwtError, JwtService, TokenType};
use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    error::{ErrorForbidden, ErrorUnauthorized},
    http::Method,
    Error, HttpMessage,
};
use futures::future::LocalBoxFuture;
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

fn required_roles(path: &str) -> Option<&'static [&'static str]> {
    if path == "/api/auth/analytics"
        || path.starts_with("/api/ip-list")
        || path.starts_with("/api/feature-flags")
        || path.starts_with("/api/cache")
        || path.starts_with("/api/idempotency")
        || path.starts_with("/api/reputation/bad-actors")
    {
        return Some(&["admin", "super_admin"]);
    }

    if path.starts_with("/api/suspensions") || path.starts_with("/api/disputes") {
        return Some(&["admin", "super_admin", "moderator"]);
    }

    None
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
                        let has_role = claims.roles.iter().any(|role| {
                            allowed_roles.iter().any(|allowed| role.eq_ignore_ascii_case(allowed))
                        });
                        if !has_role {
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
    fn test_claims_ext_interface() {
        // This test just ensures the trait compiles
        assert!(true);
    }
}
