mod common;

use axum::http::StatusCode;

#[tokio::test]
async fn healthz_answers_when_the_database_does() {
    let Some(db) = common::Db::new().await else { return };
    let (status, body) = common::get(&db.app(), "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
    db.drop().await;
}

#[tokio::test]
async fn migrations_create_the_instance_record() {
    let Some(db) = common::Db::new().await else { return };
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM instance").fetch_one(&db.pool).await.unwrap();
    assert_eq!(n, 1);
    db.drop().await;
}
