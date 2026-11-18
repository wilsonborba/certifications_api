use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
};
use certifications_api::{
    core::Result,
    domain::auth::{reject_caller_authority_fields, require_owner, require_owner_or_admin},
    presentation::auth::{AdminUser, SessionUser, admin_guard},
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn test_router() -> Router {
    Router::new()
        .route(
            "/public/ping",
            get(|| async { Json(json!({ "guest": true })) }),
        )
        .route(
            "/protected/items/{owner_id}",
            get(
                |SessionUser(caller): SessionUser,
                 axum::extract::Path(owner_id): axum::extract::Path<String>| async move {
                    require_owner(&caller, &owner_id)?;
                    Ok::<_, certifications_api::core::Error>(Json(json!({
                        "user_id": caller.user_id,
                        "display_name": caller.display_name,
                    })))
                },
            )
            .post(
                |SessionUser(caller): SessionUser,
                 axum::extract::Path(owner_id): axum::extract::Path<String>,
                 Json(body): Json<Value>| async move {
                    reject_caller_authority_fields(&body)?;
                    require_owner_or_admin(&caller, &owner_id)?;
                    Ok::<_, certifications_api::core::Error>(Json(json!({
                        "owner_id": caller.user_id,
                        "ok": true,
                    })))
                },
            ),
        )
        .route(
            "/admin/settings",
            post(|AdminUser(caller): AdminUser| async move {
                Result::Ok(Json(json!({
                    "admin": true,
                    "user_id": caller.map(|c| c.user_id),
                })))
            }),
        )
        .layer(axum::middleware::from_fn(admin_guard))
}

#[tokio::test]
async fn session_and_admin_boundaries_enforce_identity_and_ownership() {
    let app = test_router();

    // Public route works as guest without identity headers.
    let pub_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/public/ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pub_resp.status(), StatusCode::OK);

    // Anonymous call to protected route returns 401.
    let anon = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/protected/items/u1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // Cross-user access fails with 403.
    let cross = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/protected/items/u2")
                .header("x-uuid", "u1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);

    // Owner access succeeds and decodes percent-encoded UTF-8 display name.
    let own = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/protected/items/u1")
                .header("x-uuid", "u1")
                .header("x-display-name", "Ana%20Silva")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    let own_body = http_body_util::BodyExt::collect(own.into_body())
        .await
        .unwrap()
        .to_bytes();
    let own_json: Value = serde_json::from_slice(&own_body).unwrap();
    assert_eq!(own_json["user_id"], "u1");
    assert_eq!(own_json["display_name"], "Ana Silva");

    // Caller payload cannot set owner_id or access_level.
    for field in ["owner_id", "user_id", "access_level", "is_admin"] {
        let spoof = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/protected/items/u1")
                    .header("x-uuid", "u1")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ field: "u2" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(spoof.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Admin route refuses anonymous and normal session (access_level=3) with 403.
    for headers in [vec![], vec![("x-uuid", "u1"), ("x-access-level", "3")]] {
        let mut req = Request::builder().method("POST").uri("/admin/settings");
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    // Admin route accepts access_level=1 (admin key without x-uuid).
    let adm = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/settings")
                .header("x-access-level", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(adm.status(), StatusCode::OK);
}
