-- 원본: csharp_likeactor/Scripts/db/create_accounts.sql (commit 89219b2) 에서 복사.

CREATE TABLE IF NOT EXISTS accounts (
    account_id    BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    user_id       VARCHAR(50)  NOT NULL UNIQUE,
    password_hash VARCHAR(88)  NOT NULL,
    salt          VARCHAR(88)  NOT NULL,
    status        TINYINT UNSIGNED NOT NULL DEFAULT 0,
    created_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_login_at DATETIME NULL,
    INDEX idx_user_id (user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
