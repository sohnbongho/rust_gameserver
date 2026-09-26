//! `--seed`: 테스트 계정 `user_00001` .. 을 만든다 (C# `AccountSeeder`).

use std::time::Instant;

use db::sqlx::{self, MySqlPool};

/// 원본 저장소에서 복사한 스키마 (sql/create_accounts.sql)
const CREATE_ACCOUNTS_SQL: &str = include_str!("../../../sql/create_accounts.sql");

pub async fn seed(pool: &MySqlPool, password: &str, account_count: usize) -> anyhow::Result<()> {
    tracing::info!("[Seeder] {account_count}개 계정 생성 시작...");

    sqlx::raw_sql(CREATE_ACCOUNTS_SQL).execute(pool).await?;
    tracing::info!("[Seeder] 테이블 준비 완료");

    // PBKDF2 10만 회 × 계정 수 — 순차로 하면 수 분이 걸리므로 코어 수만큼 병렬로 만든다
    let started = Instant::now();
    let client_hash = db::password::client_hash(password);
    let hashes =
        tokio::task::spawn_blocking(move || generate_hashes(&client_hash, account_count)).await?;
    tracing::info!(
        "[Seeder] 해시 생성 완료 ({:.1}s)",
        started.elapsed().as_secs_f64()
    );

    let mut inserted = 0u64;
    for (i, (hash, salt)) in hashes.iter().enumerate() {
        let index = i + 1;
        let result = sqlx::query(
            "INSERT IGNORE INTO accounts (user_id, password_hash, salt) VALUES (?, ?, ?)",
        )
        .bind(crate::client::user_id(index))
        .bind(hash)
        .bind(salt)
        .execute(pool)
        .await?;
        inserted += result.rows_affected();

        if index % 1000 == 0 {
            tracing::info!("[Seeder] {index}/{account_count}");
        }
    }

    tracing::info!("[Seeder] 완료: {inserted}개 신규 삽입 (중복 제외)");
    Ok(())
}

fn generate_hashes(client_hash: &[u8], count: usize) -> Vec<(String, String)> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = count.div_ceil(threads).max(1);

    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..count)
            .step_by(chunk)
            .map(|start| {
                let len = chunk.min(count - start);
                scope.spawn(move || {
                    (0..len)
                        .map(|_| db::password::generate_stored_hash(client_hash))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("해시 생성 스레드 패닉"))
            .collect()
    })
}
