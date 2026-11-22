use crate::auth::jwt_service::{Claims, JwtError, JwtService, TokenType};
use crate::auth::operator_session::{OperatorSessionError, OperatorSessionStore};
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

/// Header carrying the opaque operator session token for privileged actions.
pub const OPERATOR_SESSION_HEADER: &str = "X-Operator-Session";

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
            | (&Method::POST, "/api/auth/recovery/start")
            | (&Method::POST, "/api/auth/recovery/complete")
            | (&Method::POST, "/api/auth/recovery/appeal")
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

/// Privileged operator actions that require a bounded, revocable session.
///
/// Returns the scope that a valid operator session must cover for the given
/// request, or `None` when the route is not a privileged operator action.
fn required_operator_scope(method: &Method, path: &str) -> Option<&'static str> {
    if path.starts_with("/api/operator") {
        return Some("operator");
    }
    if path.starts_with("/api/ip-list")
        || path.starts_with("/api/feature-flags")
        || path.starts_with("/api/cache")
        || path.starts_with("/api/idempotency")
        || path.starts_with("/api/reputation/bad-actors")
    {
        return Some("admin");
    }
    if path.starts_with("/api/suspensions") || path.starts_with("/api/disputes") {
        return Some("moderation");
    }
    if method != Method::GET && path.starts_with("/api/soroban") {
        return Some("soroban");
    }
    None
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
    operator_sessions: Option<Rc<OperatorSessionStore>>,
}

impl AuthMiddleware {
    pub fn new(jwt_service: JwtService) -> Self {
        Self {
            jwt_service: Rc::new(jwt_service),
            operator_sessions: None,
        }
    }

    /// Attach the operator session store used to gate privileged actions.
    pub fn with_operator_sessions(mut self, store: OperatorSessionStore) -> Self {
        self.operator_sessions = Some(Rc::new(store));
        self
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
            operator_sessions: self.operator_sessions.clone(),
        }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: Rc<S>,
    jwt_service: Rc<JwtService>,
    operator_sessions: Option<Rc<OperatorSessionStore>>,
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
        let operator_sessions = self.operator_sessions.clone();

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

                    // ── Operator session gate ────────────────────────────────
                    // Privileged actions must present a bounded, revocable
                    // operator session bound to the actor, purpose, scope,
                    // expiry and approval reference. Checked immediately before
                    // the handler runs (i.e. before submission).
                    if let Some(required_scope) = required_operator_scope(req.method(), req.path()) {
                        let store = match operator_sessions.as_ref() {
                            Some(s) => s,
                            None => {
                                warn!(
                                    user_id = %claims.sub,
                                    path = %req.path(),
                                    "Privileged action attempted without operator session store"
                                );
                                return Err(ErrorForbidden("Operator session required"));
                            }
                        };

                        let session_token = req
                            .headers()
                            .get(OPERATOR_SESSION_HEADER)
                            .and_then(|h| h.to_str().ok())
                            .map(str::to_owned);

                        let session_token = match session_token {
                            Some(t) => t,
                            None => {
                                warn!(
                                    user_id = %claims.sub,
                                    path = %req.path(),
                                    "Missing operator session token"
                                );
                                return Err(ErrorForbidden("Operator session required"));
                            }
                        };

                        match store.validate(&session_token, &claims.sub, required_scope) {
                            Ok(session) => {
                                debug!(
                                    user_id = %claims.sub,
                                    session_id = %session.session_id,
                                    purpose = %session.purpose,
                                    scope = %session.scope,
                                    approval_ref = %session.approval_ref,
                                    "Operator session validated"
                                );
                                req.extensions_mut().insert(session);
                            }
                            Err(e) => {
                                warn!(
                                    user_id = %claims.sub,
                                    path = %req.path(),
                                    error = %e,
                                    "Operator session rejected"
                                );
                                return Err(match e {
                                    OperatorSessionError::Expired => {
                                        ErrorForbidden("Operator session expired")
                                    }
                                    OperatorSessionError::Revoked => {
                                        ErrorForbidden("Operator session revoked")
                                    }
                                    OperatorSessionError::WrongScope => {
                                        ErrorForbidden("Operator session scope mismatch")
                                    }
                                    OperatorSessionError::ActorMismatch => {
                                        ErrorForbidden("Operator session actor mismatch")
                                    }
                                    OperatorSessionError::NotFound => {
                                        ErrorForbidden("Operator session not found")
                                    }
                                });
                            }
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
    use crate::auth::operator_session::{OperatorSessionRequest, OperatorSessionStore};
    use std::time::Duration;

    fn store() -> OperatorSessionStore {
        OperatorSessionStore::new()
    }

    fn issue(store: &OperatorSessionStore, actor: &str, scope: &str) -> String {
        store
            .issue(OperatorSessionRequest {
                actor: actor.to_string(),
                purpose: "manual-payout".to_string(),
                scope: scope.to_string(),
                ttl: Duration::from_secs(60),
                approval_ref: "approval-123".to_string(),
            })
            .expect("session issued")
    }

    #[test]
    fn test_claims_ext_interface() {
        // This test just ensures the trait compiles
        assert!(true);
    }

    #[test]
    fn operator_scope_mapping() {
        assert_eq!(
            required_operator_scope(&Method::POST, "/api/operator/payout"),
            Some("operator")
        );
        assert_eq!(
            required_operator_scope(&Method::POST, "/api/feature-flags"),
            Some("admin")
        );
        assert_eq!(
            required_operator_scope(&Method::POST, "/api/disputes/1/resolve"),
            Some("moderation")
        );
        assert_eq!(
            required_operator_scope(&Method::POST, "/api/soroban/submit"),
            Some("soroban")
        );
        assert_eq!(
            required_operator_scope(&Method::GET, "/api/soroban/status"),
            None
        );
        assert_eq!(required_operator_scope(&Method::GET, "/api/tournaments"), None);
    }

    #[test]
    fn valid_session_is_accepted() {
        let store = store();
        let token = issue(&store, "actor-1", "operator");
        let session = store
            .validate(&token, "actor-1", "operator")
            .expect("valid session");
        assert_eq!(session.actor, "actor-1");
        assert_eq!(session.scope, "operator");
        assert_eq!(session.approval_ref, "approval-123");
    }

    #[test]
    fn expired_session_is_rejected() {
        let store = store();
        let token = store
            .issue(OperatorSessionRequest {
                actor: "actor-1".to_string(),
                purpose: "manual-payout".to_string(),
                scope: "operator".to_string(),
                ttl: Duration::from_millis(1),
                approval_ref: "approval-123".to_string(),
            })
            .expect("session issued");
        std::thread::sleep(Duration::from_millis(5));
        let err = store.validate(&token, "actor-1", "operator").unwrap_err();
        assert_eq!(err, OperatorSessionError::Expired);
    }

    #[test]
    fn revoked_session_is_rejected() {
        let store = store();
        let token = issue(&store, "actor-1", "operator");
        let session = store
            .validate(&token, "actor-1", "operator")
            .expect("valid session");
        store.revoke(&session.session_id);
        let err = store.validate(&token, "actor-1", "operator").unwrap_err();
        assert_eq!(err, OperatorSessionError::Revoked);
    }

    #[test]
    fn wrong_scope_is_rejected() {
        let store = store();
        let token = issue(&store, "actor-1", "operator");
        let err = store.validate(&token, "actor-1", "admin").unwrap_err();
        assert_eq!(err, OperatorSessionError::WrongScope);
    }

    #[test]
    fn wrong_actor_is_rejected() {
        let store = store();
        let token = issue(&store, "actor-1", "operator");
        let err = store.validate(&token, "actor-2", "operator").unwrap_err();
        assert_eq!(err, OperatorSessionError::ActorMismatch);
    }

    #[test]
    fn unknown_session_is_rejected() {
        let store = store();
        let err = store
            .validate("not-a-real-token", "actor-1", "operator")
            .unwrap_err();
        assert_eq!(err, OperatorSessionError::NotFound);
    }
}
