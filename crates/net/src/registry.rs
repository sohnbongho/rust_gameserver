//! 접속 중인 세션 목록과 user_id → 세션 매핑 (C# `UserObjectPoolManager` 의 `_activeSessions` / `_authenticatedSessions`).
//! LoginServer 와 GameServer 가 같이 쓴다.
//!
//! 세션 자체의 상태는 각 세션 task 가 소유한다. 여기에는 AdminApi 조회·강제 종료와 중복 로그인 킥에
//! 필요한 최소 정보만 둔다.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u64,
    pub user_id: Option<String>,
}

impl SessionInfo {
    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
}

#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<u64, Entry>,
    authenticated: HashMap<String, u64>,
}

struct Entry {
    user_id: Option<String>,
    kick: CancellationToken,
}

impl SessionRegistry {
    pub fn register(&self, session_id: u64, kick: CancellationToken) {
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.insert(
            session_id,
            Entry {
                user_id: None,
                kick,
            },
        );
    }

    /// 세션 종료 시 호출. 인증 매핑이 이 세션을 가리키고 있을 때만 함께 지운다.
    pub fn remove(&self, session_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.sessions.remove(&session_id) else {
            return;
        };
        if let Some(user_id) = entry.user_id
            && inner.authenticated.get(&user_id) == Some(&session_id)
        {
            inner.authenticated.remove(&user_id);
        }
    }

    /// 인증 완료 등록. 같은 user_id 의 기존 세션이 있으면 강제 종료시킨다 (중복 로그인 킥).
    pub fn authenticate(&self, session_id: u64, user_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.sessions.get_mut(&session_id) {
            entry.user_id = Some(user_id.to_owned());
        }

        let old = inner.authenticated.insert(user_id.to_owned(), session_id);
        if let Some(old_id) = old
            && old_id != session_id
            && let Some(old_entry) = inner.sessions.get(&old_id)
        {
            tracing::info!(
                user_id,
                old_session = old_id,
                new_session = session_id,
                "중복 로그인, 기존 세션 종료"
            );
            old_entry.kick.cancel();
        }
    }

    /// AdminApi 강제 종료. 세션이 없으면 false.
    pub fn disconnect(&self, session_id: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        match inner.sessions.get(&session_id) {
            Some(entry) => {
                entry.kick.cancel();
                true
            }
            None => false,
        }
    }

    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().sessions.len()
    }

    pub fn snapshot(&self) -> Vec<SessionInfo> {
        let inner = self.inner.lock().unwrap();
        inner
            .sessions
            .iter()
            .map(|(&session_id, entry)| SessionInfo {
                session_id,
                user_id: entry.user_id.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_login_kicks_previous_session() {
        let registry = SessionRegistry::default();
        let (first, second) = (CancellationToken::new(), CancellationToken::new());
        registry.register(1, first.clone());
        registry.register(2, second.clone());

        registry.authenticate(1, "user_00001");
        assert!(!first.is_cancelled());

        registry.authenticate(2, "user_00001");
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());

        // 킥당한 세션이 나중에 정리되어도 새 세션의 매핑은 남는다
        registry.remove(1);
        registry.register(3, CancellationToken::new());
        registry.authenticate(3, "user_00001");
        assert!(second.is_cancelled());
    }

    #[test]
    fn remove_clears_authenticated_mapping() {
        let registry = SessionRegistry::default();
        let first = CancellationToken::new();
        registry.register(1, first.clone());
        registry.authenticate(1, "user_00001");
        registry.remove(1);
        assert_eq!(registry.count(), 0);

        registry.register(2, CancellationToken::new());
        registry.authenticate(2, "user_00001");
        assert!(!first.is_cancelled());
    }

    #[test]
    fn disconnect_unknown_session() {
        let registry = SessionRegistry::default();
        assert!(!registry.disconnect(42));
        let token = CancellationToken::new();
        registry.register(42, token.clone());
        assert!(registry.disconnect(42));
        assert!(token.is_cancelled());
    }
}
