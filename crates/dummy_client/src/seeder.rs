//! `--seed`: 테스트 계정 `user_00001` .. 을 만든다 (C# `AccountSeeder`).

use std::time::Instant;

use db::sqlx::{self, MySqlPool};

/// 한 번에 해시를 만들고 INSERT 하는 계정 수 (진행 로그 단위)
const SEED_BATCH_SIZE: usize = 1000;

/// 원본 저장소에서 복사한 스키마 (sql/create_accounts.sql)
const CREATE_ACCOUNTS_SQL: &str = include_str!("../../../sql/create_accounts.sql");

pub async fn seed(pool: &MySqlPool, password: &str, account_count: usize) -> anyhow::Result<()> {
    tracing::info!("[Seeder] {account_count}개 계정 생성 시작...");

    sqlx::raw_sql(CREATE_ACCOUNTS_SQL).execute(pool).await?;
    tracing::info!("[Seeder] 테이블 준비 완료");

    // PBKDF2 10만 회 × 계정 수 — 코어 수만큼 병렬로 만들고, 중단해도 진행분이 남도록 배치 단위로 바로 넣는다
    let started = Instant::now();
    let client_hash = db::password::client_hash(password);
    let mut inserted = 0u64;

    for batch_start in (1..=account_count).step_by(SEED_BATCH_SIZE) {
        let batch_len = SEED_BATCH_SIZE.min(account_count + 1 - batch_start);
        let hashes =
            tokio::task::spawn_blocking(move || generate_hashes(&client_hash, batch_len)).await?;

        for (offset, (hash, salt)) in hashes.iter().enumerate() {
            let result = sqlx::query(
                "INSERT IGNORE INTO accounts (user_id, password_hash, salt) VALUES (?, ?, ?)",
            )
            .bind(crate::client::user_id(batch_start + offset))
            .bind(hash)
            .bind(salt)
            .execute(pool)
            .await?;
            inserted += result.rows_affected();
        }

        tracing::info!(
            "[Seeder] {}/{account_count} ({:.1}s)",
            batch_start + batch_len - 1,
            started.elapsed().as_secs_f64()
        );
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
