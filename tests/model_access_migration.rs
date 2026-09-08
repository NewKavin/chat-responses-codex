mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chat_responses_codex::server::build_router;
use chat_responses_codex::state::{AppConfig, AppState, ModelGroup};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn fresh() -> (AppState, String, String) {
    let url = common::oidc::database_url().expect("isolated test database required");
    assert!(common::oidc::ensure_database(&url).await, "isolated database unavailable");
    AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();
    common::oidc::reset_portal_tables(&url).await;
    let state = AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();
    let store = state.portal_store().unwrap();
    let user = store.create_user_with_identity(
        "owner@example.test", None, None, "test", "owner",
    ).await.unwrap();
    store.create_model_group(&ModelGroup {
        id: "extra".into(), name: "Extra".into(), description: None,
        allowed_models: vec!["extra-model".into()], created_at: 0, updated_at: 0,
    }).await.unwrap();
    store.grant_user_model_group(&user.id, "extra", Some("admin")).await.unwrap();
    let raw_session = "model-access-test-session";
    let hash = format!("{:x}", Sha256::digest(raw_session.as_bytes()));
    store.create_session(&hash, &user.id,
        chat_responses_codex::state::unix_seconds() as i64 + 3600, None, None,
    ).await.unwrap();
    (state, user.id, format!("portal_session={raw_session}"))
}

async fn request(state: &AppState, cookie: &str, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = build_router(state.clone()).oneshot(Request::builder()
        .method(method).uri(path).header("cookie", cookie).header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn new_portal_key_inherits_all_owner_groups() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (status, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({"label":"test"})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["model_access"]["mode"], "inherit");
    let id = created["downstream_id"].as_str().unwrap();
    let downstream = state.downstream_config(id).await.unwrap();
    let allowed = state.effective_model_allowlist(&downstream).await.unwrap();
    assert!(allowed.contains(&"extra-model".to_string()), "{allowed:?}");
    assert!(!allowed.contains(&"*".to_string()), "{allowed:?}");
}

#[tokio::test]
async fn user_model_access_does_not_require_a_default_key() {
    let _guard = common::oidc::lock().await;
    let (state, user_id, cookie) = fresh().await;
    let (status, body) = request(&state, &cookie, "GET", "/api/portal/model-access", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user_id"], user_id);
    assert_eq!(body["scope"], "user");
    assert_eq!(body["available_models"], json!([]));
    assert!(body["source"]["user_group_ids"].as_array().unwrap().contains(&json!("extra")));
}

#[tokio::test]
async fn deleting_a_bound_group_never_turns_into_inheritance() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (status, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({"model_group_id":"extra"})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    state.portal_store().unwrap().delete_model_group("extra").await.unwrap();
    let downstream = state.downstream_config(created["downstream_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(state.effective_model_allowlist(&downstream).await.unwrap(), ["__none__"]);
}

#[tokio::test]
async fn switching_a_key_group_changes_the_effective_policy() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (_, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({})).await;
    let id = created["downstream_id"].as_str().unwrap();
    let (status, body) = request(&state, &cookie, "PUT", &format!("/api/portal/keys/{id}/model-group"), json!({"model_group_id":"extra"})).await;
    assert!(status.is_success(), "{status} {body}");
    let downstream = state.downstream_config(id).await.unwrap();
    assert_eq!(state.effective_model_allowlist(&downstream).await.unwrap(), ["extra-model"]);
    let (_, keys) = request(&state, &cookie, "GET", "/api/portal/keys", Value::Null).await;
    assert_eq!(keys[0]["model_access"], json!({"mode":"group","group_id":"extra"}));
}

#[tokio::test]
async fn deleting_the_default_key_revokes_its_policy_without_deleting_history() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (_, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({})).await;
    let id = created["downstream_id"].as_str().unwrap();
    let (status, body) = request(&state, &cookie, "DELETE", &format!("/api/portal/keys/{id}"), Value::Null).await;
    assert!(status.is_success(), "{status} {body}");
    let policies = state.portal_store().unwrap().access_policies(&[id.to_string()]).await.unwrap();
    assert_eq!(policies[id].mode, chat_responses_codex::state::AccessMode::Deny);
    assert!(policies[id].owner_user_id.is_none());
    assert_eq!(policies[id].subject_kind, "portal");
}

#[tokio::test]
async fn old_binding_permissions_are_migrated_before_the_old_column_is_retired() {
    use chat_responses_codex::keys::generate_downstream_key;
    use chat_responses_codex::state::DownstreamConfig;
    let _guard = common::oidc::lock().await;
    let (state, user_id, _) = fresh().await;
    let key = generate_downstream_key("gw");
    state.insert_downstream(DownstreamConfig {
        id:"legacy-key".into(),name:"Legacy".into(),hash:key.hash,plaintext_key:Some(key.plaintext),
        model_group_id:Some("all".into()),active:true,..Default::default()
    }).await.unwrap();
    let store = state.portal_store().unwrap();
    let client = store.get_client().await.unwrap();
    client.execute("INSERT INTO portal_user_downstreams(user_id,downstream_id,model_group_id,is_default) VALUES($1,'legacy-key','extra',TRUE)", &[&user_id]).await.unwrap();
    client.execute("DELETE FROM portal_user_model_groups WHERE user_id=$1", &[&user_id]).await.unwrap();
    client.batch_execute("DROP TABLE downstream_access_policies, downstream_access_migrations").await.unwrap();
    drop(client);
    let url = common::oidc::database_url().unwrap();
    let migrated = AppState::load_from_database_url(&url,AppConfig::default()).await.unwrap();
    let downstream = migrated.downstream_config("legacy-key").await.unwrap();
    assert_eq!(migrated.effective_model_allowlist(&downstream).await.unwrap(),["extra-model"]);
    assert!(migrated.portal_store().unwrap().user_can_access_model_group(&user_id,"extra").await.unwrap());
    migrated.portal_store().unwrap().revoke_user_model_group(&user_id,"extra").await.unwrap();
    let reloaded = AppState::load_from_database_url(&url,AppConfig::default()).await.unwrap();
    assert_eq!(reloaded.effective_model_allowlist(&downstream).await.unwrap(),["__none__"]);
}

#[tokio::test]
async fn rotation_preserves_account_configuration_and_access_mode() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (_, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({"label":"environment"})).await;
    let old_id = created["downstream_id"].as_str().unwrap();
    let mut old = state.downstream_config(old_id).await.unwrap();
    old.max_concurrency = 37;
    old.request_quota_requests = Some(17);
    old.ip_allowlist = vec!["127.0.0.1".into()];
    state.update_downstream(old_id,old).await.unwrap();
    let (status, rotated) = request(&state, &cookie,"POST",&format!("/api/portal/keys/{old_id}/rotate"),Value::Null).await;
    assert!(status.is_success(),"{status} {rotated}");
    let new_id = rotated["downstream_id"].as_str().unwrap();
    let new_key = state.downstream_config(new_id).await.unwrap();
    assert_eq!(new_key.max_concurrency,37);
    assert_eq!(new_key.request_quota_requests,Some(17));
    assert_eq!(new_key.ip_allowlist,["127.0.0.1"]);
    let policies = state.portal_store().unwrap().access_policies(&[new_id.into()]).await.unwrap();
    assert_eq!(policies[new_id].mode,chat_responses_codex::state::AccessMode::Inherit);
    assert!(state.downstream_for_secret(created["plaintext_key"].as_str().unwrap()).await.is_none());
}

async fn admin_request(state: &AppState, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let app = build_router(state.clone());
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/api/admin/login")
        .header("content-type","application/json")
        .body(Body::from(json!({"username":state.config.admin_username,"password":state.config.admin_password}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    let bytes = to_bytes(response.into_body(),usize::MAX).await.unwrap();
    let login: Value = serde_json::from_slice(&bytes).unwrap();
    let response = app.oneshot(Request::builder().method(method).uri(path)
        .header("authorization",format!("Bearer {}",login["token"].as_str().unwrap()))
        .header("content-type","application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(),usize::MAX).await.unwrap();
    (status,serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn admin_configuration_updates_policy_and_expiration_together() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (_, key) = request(&state,&cookie,"POST","/api/portal/keys",json!({})).await;
    let id = key["downstream_id"].as_str().unwrap();
    let expiry = chat_responses_codex::state::unix_seconds()+3600;
    let (status,body) = admin_request(&state,"PUT",&format!("/api/admin/downstreams/{id}"),json!({
        "model_access":{"mode":"group","group_id":"extra"},"expires_at":expiry
    })).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let downstream = state.downstream_config(id).await.unwrap();
    assert_eq!(downstream.expires_at,Some(expiry));
    assert_eq!(state.effective_model_allowlist(&downstream).await.unwrap(),["extra-model"]);
}

#[tokio::test]
async fn admin_batch_model_group_uses_the_same_policy_as_single_update() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (_, key) = request(&state,&cookie,"POST","/api/portal/keys",json!({})).await;
    let id = key["downstream_id"].as_str().unwrap();
    let (status,body) = admin_request(&state,"POST","/api/admin/downstreams/batch-update",json!({
        "ids":[id],"updates":{"model_group_id":"extra"}
    })).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    assert_eq!(body["updated"],json!([id]));
    let downstream = state.downstream_config(id).await.unwrap();
    assert_eq!(state.effective_model_allowlist(&downstream).await.unwrap(),["extra-model"]);
}

#[tokio::test]
async fn legacy_login_provisions_a_durable_owner_and_cannot_reclaim_an_unbound_key() {
    let _guard = common::oidc::lock().await;
    let (state, _, _) = fresh().await;
    let key = chat_responses_codex::keys::generate_downstream_key("gw");
    state.insert_downstream(chat_responses_codex::state::DownstreamConfig {
        id:"legacy".into(),name:"Legacy".into(),hash:key.hash,plaintext_key:Some(key.plaintext),
        model_group_id:Some("extra".into()),active:true,..Default::default()
    }).await.unwrap();
    let store = state.portal_store().unwrap();
    let user = store.ensure_user_for_downstream("legacy",None).await.unwrap();
    let policy = store.access_policies(&["legacy".into()]).await.unwrap();
    assert_eq!(policy["legacy"].owner_user_id.as_deref(),Some(user.as_str()));
    store.remove_downstream_binding(&user,"legacy").await.unwrap();
    assert!(store.ensure_user_for_downstream("legacy",None).await.is_err());
}

#[tokio::test]
async fn portal_login_token_cannot_access_admin_even_when_the_login_id_matches_admin() {
    let _guard = common::oidc::lock().await;
    let (state, _, _) = fresh().await;
    let key = chat_responses_codex::keys::generate_downstream_key("key");
    state.insert_downstream(chat_responses_codex::state::DownstreamConfig {
        id:state.config.admin_username.clone(),name:"Portal credential".into(),hash:key.hash,
        plaintext_key:Some(key.plaintext.clone()),model_group_id:Some("extra".into()),active:true,
        ..Default::default()
    }).await.unwrap();
    let (status, login) = request(&state,"","POST","/api/portal/login",json!({
        "employee_id":state.config.admin_username,"key":key.plaintext
    })).await;
    assert_eq!(status,StatusCode::OK);
    let response = build_router(state).oneshot(Request::builder().uri("/api/admin/downstreams")
        .header("authorization",format!("Bearer {}",login["token"].as_str().unwrap()))
        .body(Body::empty()).unwrap()).await.unwrap();
    assert!(matches!(response.status(),StatusCode::UNAUTHORIZED|StatusCode::FORBIDDEN));
}

#[tokio::test]
async fn user_group_replacement_rolls_back_all_changes_if_a_write_fails() {
    let _guard = common::oidc::lock().await;
    let (state,user_id,_) = fresh().await;
    let store = state.portal_store().unwrap();
    store.create_model_group(&ModelGroup {
        id:"new-group".into(),name:"New group".into(),description:None,
        allowed_models:vec!["new-model".into()],created_at:0,updated_at:0,
    }).await.unwrap();
    let client = store.get_client().await.unwrap();
    client.batch_execute("CREATE OR REPLACE FUNCTION reject_access_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected revoke failure'; END $$;
        CREATE TRIGGER reject_access_revoke BEFORE DELETE ON portal_user_model_groups FOR EACH ROW EXECUTE FUNCTION reject_access_revoke();").await.unwrap();
    drop(client);
    let (status,_) = admin_request(&state,"PUT",&format!("/api/admin/portal/users/{user_id}/model-groups"),json!({"model_group_ids":["new-group"]})).await;
    assert!(status.is_server_error());
    assert!(!store.user_can_access_model_group(&user_id,"new-group").await.unwrap());
    assert!(store.user_can_access_model_group(&user_id,"extra").await.unwrap());
}

/// 构造旧库形态：deny-all 的 L3 + 业务组绑定 → 迁移分类 review_required。
async fn legacy_review_required_fixture() -> (String, String) {
    let url = common::oidc::database_url().expect("isolated test database required");
    assert!(common::oidc::ensure_database(&url).await, "isolated database unavailable");
    // reset 会 DROP portal/组表；先 load 建表，reset 后再次 load 重建
    // （空库迁移无操作），然后手工插入旧库形态数据，最后测试内第三次
    // load 触发启动迁移对旧数据分类。
    let _ = AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();
    common::oidc::reset_portal_tables(&url).await;
    let _ = AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();
    let client = common::oidc::connect(&url)
        .await
        .expect("connect isolated test database");
    client
        .batch_execute(
            "INSERT INTO model_groups (id, name, description, allowed_models) VALUES
               ('extra','Extra','','[\"extra-model\"]'::jsonb)
               ON CONFLICT (id) DO NOTHING;
             INSERT INTO portal_users (id, email, disabled) VALUES ('owner-u','owner@example.test',false);
             INSERT INTO downstreams (id, name, hash, rate_limit_enabled, per_minute_limit,
               max_concurrency, active, billing_mode, model_group_id, is_portal_key)
               VALUES ('key-review','Review Key','hash-placeholder',true,10,2,true,'request','deny-all',true);
             INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default, model_group_id)
               VALUES ('owner-u','key-review',true,'extra');",
        )
        .await
        .expect("seed legacy review-required fixture");
    drop(client);
    (url, "key-review".to_string())
}

/// P10：待确认记录可从摘要/预览看到，选中应用后用户获得待补组授权、
/// 密钥置显式 inherit，门户与请求立即生效。
#[tokio::test]
async fn access_migration_review_required_preview_and_apply() {
    let _guard = common::oidc::lock().await;
    let (url, key_id) = legacy_review_required_fixture().await;
    let state = AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();

    let (status, body) = admin_request(&state, "GET", "/api/admin/portal/users/access-migration", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["pending"], 1, "{body}");
    let item = &body["pending"][0];
    assert_eq!(item["downstream_id"], key_id);
    assert_eq!(item["classification"], "review_required");
    assert_eq!(item["owner_user_id"], "owner-u");
    assert!(item["candidate_group_ids"]
        .as_array()
        .unwrap()
        .contains(&json!("extra")), "{item}");
    let revision = item["revision"].as_i64().unwrap();
    let fingerprint = item["fingerprint"].as_str().unwrap().to_string();

    // 应用前：密钥 deny、用户无 extra 授权。
    let store = state.portal_store().unwrap();
    assert!(
        !store.user_can_access_model_group("owner-u", "extra").await.unwrap(),
        "binding alone must not grant the group"
    );
    let (status, body) = admin_request(
        &state,
        "POST",
        "/api/admin/portal/users/access-migration",
        json!({
            "items": [{
                "downstream_id": key_id,
                "candidate_group_ids": ["extra"],
                "set_inherit": true,
                "expected_revision": revision,
                "expected_fingerprint": fingerprint,
            }]
        }),
    )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"], json!([key_id]), "{body}");

    // 应用后：授权补齐、密钥 inherit、记录已解决、目录立即可见。
    assert!(
        store.user_can_access_model_group("owner-u", "extra").await.unwrap(),
        "apply must grant the candidate group"
    );
    let policies = store.access_policies(std::slice::from_ref(&key_id)).await.unwrap();
    assert_eq!(policies[&key_id].mode, chat_responses_codex::state::AccessMode::Inherit);
    let (status, body) = admin_request(&state, "GET", "/api/admin/portal/users/access-migration", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["summary"]["pending"], 0, "{body}");
    let downstream = state.downstream_config(&key_id).await.unwrap();
    let allowed = state.effective_model_allowlist(&downstream).await.unwrap();
    assert!(allowed.contains(&"extra-model".to_string()), "{allowed:?}");
    assert!(!allowed.contains(&"*".to_string()), "{allowed:?}");
}

/// P09/P10：预览失效（用户授权/组内容/策略 revision 变化）→ 提交 409，
/// 不能按过期预览放权；重叠提交幂等拒绝。
#[tokio::test]
async fn access_migration_apply_rejects_stale_preview() {
    let _guard = common::oidc::lock().await;
    let (url, key_id) = legacy_review_required_fixture().await;
    let state = AppState::load_from_database_url(&url, AppConfig::default()).await.unwrap();
    let (status, body) = admin_request(&state, "GET", "/api/admin/portal/users/access-migration", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let item = &body["pending"][0];
    let revision = item["revision"].as_i64().unwrap();
    let fingerprint = item["fingerprint"].as_str().unwrap().to_string();

    // 预览后、提交前改变用户授权 → 指纹不一致。
    let store = state.portal_store().unwrap();
    store.grant_user_model_group("owner-u", "premium", Some("admin")).await.unwrap();

    let (status, body) = admin_request(
        &state,
        "POST",
        "/api/admin/portal/users/access-migration",
        json!({
            "items": [{
                "downstream_id": key_id,
                "candidate_group_ids": ["extra"],
                "set_inherit": true,
                "expected_revision": revision,
                "expected_fingerprint": fingerprint,
            }]
        }),
    )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"], json!([]), "{body}");
    assert_eq!(body["failed"][0]["code"], "conflict", "{body}");

    // 未按过期预览放权：策略仍是 deny，用户没有 extra 授权。
    assert!(
        !store.user_can_access_model_group("owner-u", "extra").await.unwrap(),
        "stale preview must not grant access"
    );
    let policies = store.access_policies(std::slice::from_ref(&key_id)).await.unwrap();
    assert_eq!(policies[&key_id].mode, chat_responses_codex::state::AccessMode::Deny);
}

/// 跨用户批量模型组：replace 原子替换、add 只增、remove 只撤；
/// basic 不可被 remove；部分失败按实际结果返回。
#[tokio::test]
async fn admin_batch_model_groups_applies_per_user_atomically() {
    let _guard = common::oidc::lock().await;
    let (state, owner_id, _cookie) = fresh().await;
    let store = state.portal_store().unwrap();
    let second = store.create_user_with_identity(
        "second@example.test", None, None, "test", "second",
    ).await.unwrap();

    let (status, body) = admin_request(&state, "POST", "/api/admin/portal/users/batch-model-groups", json!({
        "user_ids": [owner_id, second.id],
        "op": "add",
        "model_group_ids": ["extra"],
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"].as_array().unwrap().len(), 2, "{body}");
    assert!(store.user_can_access_model_group(&owner_id, "extra").await.unwrap());
    assert!(store.user_can_access_model_group(&second.id, "extra").await.unwrap());

    // replace：只保留 extra（basic 恒在，其余撤销）。
    store.grant_user_model_group(&owner_id, "premium", Some("admin")).await.unwrap();
    let (status, body) = admin_request(&state, "POST", "/api/admin/portal/users/batch-model-groups", json!({
        "user_ids": [owner_id],
        "op": "replace",
        "model_group_ids": ["premium"],
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"], json!([owner_id]), "{body}");
    assert!(!store.user_can_access_model_group(&owner_id, "extra").await.unwrap());
    assert!(store.user_can_access_model_group(&owner_id, "premium").await.unwrap());

    // remove basic 被拒绝 → 失败项保留。
    let (status, body) = admin_request(&state, "POST", "/api/admin/portal/users/batch-model-groups", json!({
        "user_ids": [owner_id],
        "op": "remove",
        "model_group_ids": ["basic"],
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["updated"], json!([]), "{body}");
    assert_eq!(body["failed"][0]["id"], owner_id, "{body}");
}

/// 设计 5.4：quota/models 显式 downstream_id 必须属于当前 principal，
/// 越权直接 403 且不回退默认；原数组响应（models）保持数组结构并通过
/// X-Portal-Downstream-Id 头回显作用域。
#[tokio::test]
async fn portal_scope_endpoints_validate_explicit_key_ownership() {
    let _guard = common::oidc::lock().await;
    let (state, _, cookie) = fresh().await;
    let (status, created) = request(&state, &cookie, "POST", "/api/portal/keys", json!({
        "label": "scope-a", "model_access": {"mode": "inherit"}
    })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let key_a = created["downstream_id"].as_str().unwrap().to_string();

    // 另一个用户：无权访问 key_a。
    let store = state.portal_store().unwrap();
    let other = store.create_user_with_identity(
        "other-scope@example.test", None, None, "test", "other-scope",
    ).await.unwrap();
    let raw = "other-scope-session";
    let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
    store.create_session(&hash, &other.id,
        chat_responses_codex::state::unix_seconds() as i64 + 3600, None, None,
    ).await.unwrap();

    // 越权：显式 key_a 不属于 other → 403，不得回落默认 key。
    let (status, body) = request(&state, &format!("portal_session={raw}"), "GET",
        &format!("/api/portal/models?downstream_id={key_a}"), Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "model_group_forbidden", "{body}");

    // 越权 quota 同样拒绝。
    let (status, body) = request(&state, &format!("portal_session={raw}"), "GET",
        &format!("/api/portal/quota?downstream_id={key_a}"), Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // 合法：owner 自己可用显式 key；models 数组响应带 X-Portal-Downstream-Id 头。
    let response = build_router(state.clone()).oneshot(Request::builder()
        .method("GET").uri(format!("/api/portal/models?downstream_id={key_a}"))
        .header("cookie", &cookie).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-portal-downstream-id").and_then(|v| v.to_str().ok()),
        Some(key_a.as_str())
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.is_array(), "models response must stay an array");

    // quota 对象响应带 downstream_id 字段。
    let (status, body) = request(&state, &cookie, "GET",
        &format!("/api/portal/quota?downstream_id={key_a}"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["downstream_id"], key_a, "{body}");

    // 不存在的显式 key 不回落默认。
    let (status, _) = request(&state, &cookie, "GET",
        "/api/portal/models?downstream_id=no-such-key", Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unknown explicit key must not fall back");
}
