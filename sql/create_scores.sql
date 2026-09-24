-- 원본: csharp_likeactor/Scripts/db/create_scores.sql (commit 89219b2) 에서 복사.

CREATE TABLE IF NOT EXISTS scores (
    score_id        BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    account_id      BIGINT UNSIGNED NOT NULL,
    score           INT UNSIGNED NOT NULL,
    kill_count      INT UNSIGNED NOT NULL,
    survive_seconds INT UNSIGNED NOT NULL,
    played_at       DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_account_id (account_id),
    INDEX idx_score (score)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
