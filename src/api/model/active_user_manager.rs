use crate::model::{ProxyUserCredentials, UserConnectionPermission};
use crate::model::Config;
use crate::utils::{current_time_secs, default_grace_period_millis, default_grace_period_timeout_secs};
use jsonwebtoken::get_current_timestamp;
use log::{debug, info};
use rand::RngCore;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

const USER_CON_TTL: u64 = 10_800;  // 3 hours

pub struct UserConnectionGuard {
    manager: Arc<ActiveUserManager>,
    username: String,
}
impl Drop for UserConnectionGuard {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let username = self.username.clone();
        tokio::spawn(async move {
            manager.remove_connection(&username).await;
        });
    }
}

#[derive(Clone, Debug)]
pub struct UserSession {
    pub token: u32,
    pub virtual_id: u32,
    pub provider: String,
    pub stream_url: String,
    pub ts: u64,
    pub permission: UserConnectionPermission,
}

struct UserConnectionData {
    max_connections: u32,
    connections: u32,
    granted_grace: bool,
    grace_ts: u64,
    sessions: Vec<UserSession>,
}

impl UserConnectionData {
    fn new(connections: u32, max_connections: u32) -> Self {
        Self {
            max_connections,
            connections,
            granted_grace: false,
            grace_ts: 0,
            sessions: Vec::new(),
        }
    }
}

pub struct ActiveUserManager {
    grace_period_millis: u64,
    grace_period_timeout_secs: u64,
    log_active_user: bool,
    user: Arc<RwLock<HashMap<String, UserConnectionData>>>,
    gc_ts: Option<AtomicU64>,
}

impl ActiveUserManager {
    pub fn new(config: &Config) -> Self {
        let log_active_user = config.log.as_ref().is_some_and(|l| l.log_active_user);
        let (grace_period_millis, grace_period_timeout_secs) = config.reverse_proxy.as_ref()
            .and_then(|r| r.stream.as_ref())
            .map_or_else(|| (default_grace_period_millis(), default_grace_period_timeout_secs()), |s| (s.grace_period_millis, s.grace_period_timeout_secs));

        Self {
            grace_period_millis,
            grace_period_timeout_secs,
            log_active_user,
            user: Arc::new(RwLock::new(HashMap::new())),
            gc_ts: Some(AtomicU64::new(current_time_secs())),
        }
    }

    fn clone_inner(&self) -> Self {
        Self {
            grace_period_millis: self.grace_period_millis,
            grace_period_timeout_secs: self.grace_period_timeout_secs,
            log_active_user: self.log_active_user,
            user: Arc::clone(&self.user),
            gc_ts: None,
        }
    }

    pub async fn user_connections(&self, username: &str) -> u32 {
        if let Some(connection_data) = self.user.read().await.get(username) {
            return connection_data.connections;
        }
        0
    }

    fn check_connection_permission(&self, username: &str, connection_data: &mut UserConnectionData) -> UserConnectionPermission {
        let current_connections = connection_data.connections;

        if current_connections < connection_data.max_connections {
            // Reset grace period because user is back under max_connections
            connection_data.granted_grace = false;
            connection_data.grace_ts = 0;
            return UserConnectionPermission::Allowed;
        }

        let now = get_current_timestamp();
        // Check if user already used grace period
        if connection_data.granted_grace {
            if current_connections > connection_data.max_connections && now - connection_data.grace_ts <= self.grace_period_timeout_secs {
                // Grace timeout still active, deny connection
                debug!("User access denied, grace exhausted, too many connections: {username}");
                return UserConnectionPermission::Exhausted;
            }
            // Grace timeout expired, reset grace counters
            connection_data.granted_grace = false;
            connection_data.grace_ts = 0;
        }

        if self.grace_period_millis > 0 && current_connections == connection_data.max_connections {
            // Allow grace period once
            connection_data.granted_grace = true;
            connection_data.grace_ts = now;
            debug!("Granted grace period for user access: {username}");
            return UserConnectionPermission::GracePeriod;
        }

        // Too many connections, no grace allowed
        debug!("User access denied, too many connections: {username}");
        UserConnectionPermission::Exhausted
    }

    pub async fn connection_permission(
        &self,
        username: &str,
        max_connections: u32,
    ) -> UserConnectionPermission {
        if max_connections > 0 {
            if let Some(connection_data) = self.user.write().await.get_mut(username) {
                return self.check_connection_permission(username, connection_data);
            }
        }
        UserConnectionPermission::Allowed
    }


    pub async fn active_users(&self) -> usize {
        self.user.read().await.len()
    }

    pub async fn active_connections(&self) -> usize {
        Self::get_active_connections(&self.user).await
    }

    #[inline]
    async fn get_active_connections(user: &Arc<RwLock<HashMap<String, UserConnectionData>>>) -> usize {
        user.read().await.values().map(|c| c.connections as usize).sum()
    }

    pub async fn add_connection(&self, username: &str, max_connections: u32) -> UserConnectionGuard {
        let mut lock = self.user.write().await;
        if let Some(connection_data) = lock.get_mut(username) {
            connection_data.connections += 1;
            connection_data.max_connections = max_connections;
        } else {
            lock.insert(username.to_string(), UserConnectionData::new(1, max_connections));
        }
        drop(lock);

        self.log_active_user();

        UserConnectionGuard {
            manager: Arc::new(self.clone_inner()),
            username: username.to_string(),
        }
    }

    async fn remove_connection(&self, username: &str) {
        let mut lock = self.user.write().await;
        if let Some(connection_data) = lock.get_mut(username) {
            if connection_data.connections > 0 {
                connection_data.connections -= 1;
            }

            if connection_data.connections == 0  || connection_data.connections < connection_data.max_connections {
                // Grace timeout expired, reset grace counters
                connection_data.granted_grace = false;
                connection_data.grace_ts = 0;
            }
        }
        drop(lock);

        self.log_active_user();
    }

    fn find_user_session(token: u32, sessions: &[UserSession]) -> Option<&UserSession> {
        sessions.iter().find(|&session| session.token == token)
    }

    fn new_user_session(virtual_id: u32, provider: &str, stream_url: &str, connection_permission: UserConnectionPermission) -> UserSession {
        let session_token = rand::rng().next_u32();
        UserSession {
            token: session_token,
            virtual_id,
            provider: provider.to_string(),
            stream_url: stream_url.to_string(),
            ts: current_time_secs(),
            permission: connection_permission,
        }
    }

    pub async fn create_user_session(&self, user: &ProxyUserCredentials, virtual_id: u32, provider: &str, stream_url: &str, connection_permission: UserConnectionPermission) -> Option<u32> {
        self.gc().await;
        let mut lock = self.user.write().await;
        if let Some(connection_data) = lock.get_mut(&user.username) {
            let session = Self::new_user_session(virtual_id, provider, stream_url, connection_permission);
            let token = session.token;
            connection_data.sessions.push(session);
            Some(token)
        } else {
            let mut connection_data = UserConnectionData::new(0, user.max_connections);
            let session = Self::new_user_session(virtual_id, provider, stream_url, connection_permission);
            let token = session.token;
            connection_data.sessions.push(session);
            lock.insert(user.username.to_string(), connection_data);
            Some(token)
        }
    }

    pub async fn get_user_session(&self, username: &str, token: u32) -> Option<UserSession> {
        self.update_user_session(username, token).await
    }

    async fn update_user_session(&self, username: &str, token: u32) -> Option<UserSession> {
        let mut lock = self.user.write().await;
        if let Some(connection_data) = lock.get_mut(username) {
            if connection_data.max_connections == 0 {
                return Self::find_user_session(token, &connection_data.sessions).cloned();
            }

            // Separate mutable borrow of the session
            let mut found_session_index = None;
            for (i, session) in connection_data.sessions.iter().enumerate() {
                if session.token == token {
                    found_session_index = Some(i);
                    break;
                }
            }

            if let Some(index) = found_session_index {
                let session_permission = connection_data.sessions[index].permission;
                if session_permission == UserConnectionPermission::GracePeriod {
                    let new_permission = self.check_connection_permission(username, connection_data);
                    connection_data.sessions[index].permission = new_permission;
                }
                return Some(connection_data.sessions[index].clone());
            }
        }
        None
    }

    fn log_active_user(&self) {
        if self.log_active_user {
            let user = Arc::clone(&self.user);
            tokio::spawn(async move {
                let user_count = user.read().await.len();
                let user_connection_count = Self::get_active_connections(&user).await;
                info!("Active Users: {user_count}, Active User Connections: {user_connection_count}");
            });
        }
    }

    async fn gc(&self) {
        if let Some(gc_ts) = &self.gc_ts {
            let ts = gc_ts.load(Ordering::SeqCst);
            let now = current_time_secs();
            if  now - ts > USER_CON_TTL {
                let mut lock = self.user.write().await;
                for (_, connection_data) in lock.iter_mut() {
                    connection_data.sessions.retain(|s| now  - s.ts < USER_CON_TTL);
                }
                gc_ts.store(now, Ordering::SeqCst);
            }
        }
    }
}

//
// mod tests {
//     use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
//     use std::time::Instant;
//     use std::thread;
//
//     fn benchmark(ordering: Ordering, iterations: usize) -> u128 {
//         let counter = Arc::new(AtomicUsize::new(0));
//         let start = Instant::now();
//
//         let handles: Vec<_> = (0..32)
//             .map(|_| {
//                 let counter_ref = Arc::clone(&counter);
//                 thread::spawn(move || {
//                     for _ in 0..iterations {
//                         counter_ref.fetch_add(1, ordering);
//                     }
//                 })
//             })
//             .collect();
//


//         for handle in handles {
//             handle.join().unwrap();
//         }
//
//         let duration = start.elapsed();
//         duration.as_millis()
//     }
//
//     #[test]
//     fn test_ordering() {
//         let iterations = 1_000_000;
//
//         let time_acqrel = benchmark(Ordering::SeqCst, iterations);
//         println!("AcqRel: {} ms", time_acqrel);
//
//         let time_seqcst = benchmark(Ordering::SeqCst, iterations);
//         println!("SeqCst: {} ms", time_seqcst);
//     }
//
// }
