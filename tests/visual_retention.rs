use certifications_api::{
    dal::{Store, fsm::MemoryFsm},
    domain::{authoring::ORIGIN_CREATOR, cleanup},
};

#[tokio::test]
async fn legacy_visual_bytes_survive_until_the_asset_record_is_removed() {
    let mut store = Store::fixture().await;
    let fsm = MemoryFsm::new();
    store.fsm = fsm.clone();
    sqlx::query("INSERT INTO schema_versions(schema_name, version, status, description, created_at, published_at) VALUES('question-v2',1,'published','legacy',1,1)")
        .execute(&store.db).await.unwrap();
    sqlx::query("INSERT INTO questions(id,owner_id,origin,created_at) VALUES('legacy-question','legacy-owner',$1,1)")
        .bind(ORIGIN_CREATOR).execute(&store.db).await.unwrap();
    sqlx::query("INSERT INTO question_versions(id,question_id,version_num,language,schema_name,schema_version,couch_doc_id,created_at) VALUES('legacy-version','legacy-question',1,'en','question-v2',1,'legacy-document',1)")
        .execute(&store.db).await.unwrap();
    let reference = "visuals/legacy-version/retained.png";
    sqlx::query("INSERT INTO question_visuals(id,question_version_id,fsm_ref,mime_type,created_at) VALUES('legacy-visual','legacy-version',$1,'image/png',1)")
        .bind(reference).execute(&store.db).await.unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let bytes = png.into_inner();
    fsm.seed(reference, &bytes);

    // Schema1 assets deliberately have no inferred canonical references.
    let canonical_refs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM question_version_visual_refs WHERE visual_id='legacy-visual'",
    )
    .fetch_one(&store.db)
    .await
    .unwrap();
    assert_eq!(canonical_refs, 0);
    cleanup::delete_fsm_or_queue(&store, reference)
        .await
        .unwrap();
    assert!(
        fsm.has_blob(reference),
        "a stored legacy asset must protect its bytes without a canonical backfill"
    );
    assert_eq!(store.fsm.get_bytes(reference).await.unwrap(), bytes);
    assert!(cleanup::is_fsm_ref_in_use(&store, reference).await.unwrap());

    // Once its asset row is legitimately removed, byte cleanup can proceed.
    sqlx::query("DELETE FROM question_visuals WHERE id='legacy-visual'")
        .execute(&store.db)
        .await
        .unwrap();
    cleanup::delete_fsm_or_queue(&store, reference)
        .await
        .unwrap();
    assert!(!fsm.has_blob(reference));
}
