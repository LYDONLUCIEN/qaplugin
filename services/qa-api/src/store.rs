use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use qa_protocol::{SessionSummary, TurnRecord};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use uuid::Uuid;

pub const DEFAULT_PROMPT: &str = "请分析这张桌面截图：识别主要界面和可见文字，回答截图中的问题或任务，并给出简洁、可执行的说明。";

#[derive(Clone, Debug, Serialize)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub is_admin: bool,
}

#[derive(Clone, Debug)]
pub struct UserSecret {
    pub user: AuthUser,
    pub password_hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub owner_user_id: String,
    pub owner_username: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminUserSummary {
    pub id: String,
    pub username: String,
    pub is_admin: bool,
    pub device_count: u32,
    pub session_count: u32,
    pub turn_count: u32,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct TurnScreenshot {
    pub device_id: String,
    pub screenshot_b64: String,
    pub screenshot_mime: String,
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS users (
                id            TEXT PRIMARY KEY,
                username      TEXT NOT NULL COLLATE NOCASE UNIQUE,
                password_hash TEXT NOT NULL,
                is_admin      INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS devices (
                id         TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_devices_user
                ON devices(user_id, id);

            CREATE TABLE IF NOT EXISTS auth_sessions (
                token_hash TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_auth_sessions_user_expiry
                ON auth_sessions(user_id, expires_at);

            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                device_id   TEXT NOT NULL,
                title       TEXT NOT NULL,
                prompt      TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_device_updated
                ON sessions(device_id, updated_at DESC);

            CREATE TABLE IF NOT EXISTS turns (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                prompt          TEXT NOT NULL,
                screenshot_b64  TEXT NOT NULL,
                screenshot_mime TEXT NOT NULL,
                answer          TEXT NOT NULL DEFAULT '',
                status          TEXT NOT NULL,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_turns_session_created
                ON turns(session_id, created_at DESC);
            "#,
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn admin_user(&self) -> Result<Option<AuthUser>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                "SELECT id, username, is_admin FROM users WHERE is_admin = 1 ORDER BY created_at LIMIT 1",
                [],
                map_auth_user,
            )
            .optional()?)
    }

    pub fn create_initial_admin(&self, username: &str, password_hash: &str) -> Result<AuthUser> {
        if self.admin_user()?.is_some() {
            return Err(anyhow!("an administrator already exists"));
        }
        self.insert_user(username, password_hash, true)
    }

    pub fn create_user(&self, username: &str, password_hash: &str) -> Result<AuthUser> {
        self.insert_user(username, password_hash, false)
    }

    fn insert_user(&self, username: &str, password_hash: &str, is_admin: bool) -> Result<AuthUser> {
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        self.lock()?
            .execute(
                "INSERT INTO users (id, username, password_hash, is_admin, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, username, password_hash, is_admin as i64, timestamp],
            )
            .with_context(|| format!("failed to create user '{username}'"))?;
        self.user_by_id(&id)?
            .ok_or_else(|| anyhow!("created user was not found"))
    }

    pub fn user_by_id(&self, user_id: &str) -> Result<Option<AuthUser>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                "SELECT id, username, is_admin FROM users WHERE id = ?1",
                [user_id],
                map_auth_user,
            )
            .optional()?)
    }

    pub fn user_secret_by_username(&self, username: &str) -> Result<Option<UserSecret>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                "SELECT id, username, is_admin, password_hash FROM users WHERE username = ?1 COLLATE NOCASE",
                [username],
                |row| {
                    Ok(UserSecret {
                        user: AuthUser {
                            id: row.get(0)?,
                            username: row.get(1)?,
                            is_admin: row.get::<_, i64>(2)? != 0,
                        },
                        password_hash: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_user_password(&self, user_id: &str, password_hash: &str) -> Result<()> {
        let changed = self.lock()?.execute(
            "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
            params![password_hash, now(), user_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("user not found"));
        }
        self.delete_user_sessions(user_id)
    }

    pub fn create_auth_session(
        &self,
        token_hash: &str,
        user_id: &str,
        expires_at: i64,
    ) -> Result<()> {
        let timestamp = now();
        self.lock()?.execute(
            "INSERT INTO auth_sessions (token_hash, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, user_id, timestamp, expires_at],
        )?;
        Ok(())
    }

    pub fn user_by_auth_session(&self, token_hash: &str) -> Result<Option<AuthUser>> {
        let timestamp = now();
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                r#"SELECT u.id, u.username, u.is_admin
                   FROM auth_sessions a
                   JOIN users u ON u.id = a.user_id
                   WHERE a.token_hash = ?1 AND a.expires_at > ?2"#,
                params![token_hash, timestamp],
                map_auth_user,
            )
            .optional()?)
    }

    pub fn delete_auth_session(&self, token_hash: &str) -> Result<()> {
        self.lock()?.execute(
            "DELETE FROM auth_sessions WHERE token_hash = ?1",
            [token_hash],
        )?;
        Ok(())
    }

    pub fn delete_user_sessions(&self, user_id: &str) -> Result<()> {
        self.lock()?
            .execute("DELETE FROM auth_sessions WHERE user_id = ?1", [user_id])?;
        Ok(())
    }

    pub fn cleanup_expired_auth_sessions(&self) -> Result<()> {
        self.lock()?
            .execute("DELETE FROM auth_sessions WHERE expires_at <= ?1", [now()])?;
        Ok(())
    }

    pub fn provision_devices<'a>(
        &self,
        device_ids: impl IntoIterator<Item = &'a String>,
        admin_user_id: &str,
    ) -> Result<()> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        for device_id in device_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO devices (id, user_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
                params![device_id, admin_user_id, timestamp],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn can_access_device(&self, user: &AuthUser, device_id: &str) -> Result<bool> {
        let connection = self.lock()?;
        let allowed = if user.is_admin {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM devices WHERE id = ?1)",
                [device_id],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM devices WHERE id = ?1 AND user_id = ?2)",
                params![device_id, user.id],
                |row| row.get::<_, i64>(0),
            )?
        };
        Ok(allowed != 0)
    }

    pub fn devices_for_user(&self, user: &AuthUser) -> Result<Vec<DeviceSummary>> {
        let connection = self.lock()?;
        let sql = if user.is_admin {
            r#"SELECT d.id, d.user_id, u.username
               FROM devices d JOIN users u ON u.id = d.user_id
               ORDER BY d.id"#
        } else {
            r#"SELECT d.id, d.user_id, u.username
               FROM devices d JOIN users u ON u.id = d.user_id
               WHERE d.user_id = ?1 ORDER BY d.id"#
        };
        let mut statement = connection.prepare(sql)?;
        let rows = if user.is_admin {
            statement.query_map([], map_device)?
        } else {
            statement.query_map([&user.id], map_device)?
        };
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn assign_device(&self, device_id: &str, user_id: &str) -> Result<()> {
        if self.user_by_id(user_id)?.is_none() {
            return Err(anyhow!("user not found"));
        }
        let changed = self.lock()?.execute(
            "UPDATE devices SET user_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![user_id, now(), device_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("device not found"));
        }
        Ok(())
    }

    pub fn admin_users(&self) -> Result<Vec<AdminUserSummary>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            r#"SELECT u.id, u.username, u.is_admin, u.created_at,
                      (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id),
                      (SELECT COUNT(*) FROM sessions s JOIN devices d ON d.id = s.device_id WHERE d.user_id = u.id),
                      (SELECT COUNT(*) FROM turns t JOIN sessions s ON s.id = t.session_id JOIN devices d ON d.id = s.device_id WHERE d.user_id = u.id)
               FROM users u ORDER BY u.is_admin DESC, u.username"#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok(AdminUserSummary {
                id: row.get(0)?,
                username: row.get(1)?,
                is_admin: row.get::<_, i64>(2)? != 0,
                created_at: row.get(3)?,
                device_count: row.get::<_, i64>(4)? as u32,
                session_count: row.get::<_, i64>(5)? as u32,
                turn_count: row.get::<_, i64>(6)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_session(
        &self,
        device_id: &str,
        title: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<SessionSummary> {
        let id = Uuid::new_v4().to_string();
        let now = now();
        let title = clean(title).unwrap_or_else(|| "新会话".to_string());
        let prompt = clean(prompt).unwrap_or_else(|| DEFAULT_PROMPT.to_string());
        self.lock()?.execute(
            "INSERT INTO sessions (id, device_id, title, prompt, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, device_id, title, prompt, now],
        )?;
        self.session(device_id, &id)?
            .ok_or_else(|| anyhow!("created session was not found"))
    }

    pub fn ensure_session(
        &self,
        device_id: &str,
        session_id: Option<&str>,
    ) -> Result<SessionSummary> {
        if let Some(session_id) = session_id {
            return self
                .session(device_id, session_id)?
                .ok_or_else(|| anyhow!("unknown session"));
        }
        if let Some(session) = self.latest_session(device_id)? {
            return Ok(session);
        }
        self.create_session(device_id, None, None)
    }

    pub fn update_session(
        &self,
        device_id: &str,
        session_id: &str,
        title: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<SessionSummary> {
        let current = self
            .session(device_id, session_id)?
            .ok_or_else(|| anyhow!("unknown session"))?;
        let title = clean(title).unwrap_or(current.title);
        let prompt = clean(prompt).unwrap_or(current.prompt);
        let changed = self.lock()?.execute(
            "UPDATE sessions SET title = ?1, prompt = ?2, updated_at = ?3 WHERE id = ?4 AND device_id = ?5",
            params![title, prompt, now(), session_id, device_id],
        )?;
        if changed != 1 {
            return Err(anyhow!("session update failed"));
        }
        self.session(device_id, session_id)?
            .ok_or_else(|| anyhow!("updated session was not found"))
    }

    pub fn list_sessions(&self, device_id: &str) -> Result<Vec<SessionSummary>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            r#"
            SELECT s.id, s.device_id, s.title, s.prompt, s.created_at, s.updated_at,
                   COUNT(t.id) AS turn_count
            FROM sessions s
            LEFT JOIN turns t ON t.session_id = s.id
            WHERE s.device_id = ?1
            GROUP BY s.id, s.device_id, s.title, s.prompt, s.created_at, s.updated_at
            ORDER BY s.updated_at DESC
            "#,
        )?;
        let rows = statement.query_map([device_id], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn latest_session(&self, device_id: &str) -> Result<Option<SessionSummary>> {
        Ok(self.list_sessions(device_id)?.into_iter().next())
    }

    pub fn session(&self, device_id: &str, session_id: &str) -> Result<Option<SessionSummary>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                r#"
                SELECT s.id, s.device_id, s.title, s.prompt, s.created_at, s.updated_at,
                       COUNT(t.id) AS turn_count
                FROM sessions s
                LEFT JOIN turns t ON t.session_id = s.id
                WHERE s.device_id = ?1 AND s.id = ?2
                GROUP BY s.id, s.device_id, s.title, s.prompt, s.created_at, s.updated_at
                "#,
                params![device_id, session_id],
                map_session,
            )
            .optional()?)
    }

    pub fn create_turn(
        &self,
        session_id: &str,
        prompt: &str,
        screenshot_b64: &str,
        screenshot_mime: &str,
    ) -> Result<TurnRecord> {
        let id = Uuid::new_v4().to_string();
        let now = now();
        let connection = self.lock()?;
        connection.execute(
            r#"INSERT INTO turns
               (id, session_id, prompt, screenshot_b64, screenshot_mime, answer, status, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, '', 'uploading', ?6, ?6)"#,
            params![id, session_id, prompt, screenshot_b64, screenshot_mime, now],
        )?;
        connection.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        drop(connection);
        self.turn(&id)?
            .ok_or_else(|| anyhow!("created turn was not found"))
    }

    pub fn finish_turn(&self, turn_id: &str, answer: &str, status: &str) -> Result<()> {
        let connection = self.lock()?;
        let session_id: String = connection.query_row(
            "SELECT session_id FROM turns WHERE id = ?1",
            [turn_id],
            |row| row.get(0),
        )?;
        let timestamp = now();
        connection.execute(
            "UPDATE turns SET answer = ?1, status = ?2, updated_at = ?3 WHERE id = ?4",
            params![answer, status, timestamp, turn_id],
        )?;
        connection.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![timestamp, session_id],
        )?;
        Ok(())
    }

    pub fn turns(&self, session_id: &str) -> Result<Vec<TurnRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            r#"SELECT id, session_id, prompt, screenshot_b64, screenshot_mime,
                      answer, status, created_at, updated_at
               FROM turns WHERE session_id = ?1 ORDER BY created_at DESC"#,
        )?;
        let rows = statement.query_map([session_id], map_turn)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn turn_screenshot(&self, turn_id: &str) -> Result<Option<TurnScreenshot>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                r#"SELECT s.device_id, t.screenshot_b64, t.screenshot_mime
                   FROM turns t
                   JOIN sessions s ON s.id = t.session_id
                   WHERE t.id = ?1"#,
                [turn_id],
                |row| {
                    Ok(TurnScreenshot {
                        device_id: row.get(0)?,
                        screenshot_b64: row.get(1)?,
                        screenshot_mime: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    fn turn(&self, turn_id: &str) -> Result<Option<TurnRecord>> {
        let connection = self.lock()?;
        Ok(connection
            .query_row(
                r#"SELECT id, session_id, prompt, screenshot_b64, screenshot_mime,
                          answer, status, created_at, updated_at
                   FROM turns WHERE id = ?1"#,
                [turn_id],
                map_turn,
            )
            .optional()?)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("database lock poisoned"))
    }
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        device_id: row.get(1)?,
        title: row.get(2)?,
        prompt: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        turn_count: row.get::<_, i64>(6)? as u32,
    })
}

fn map_turn(row: &Row<'_>) -> rusqlite::Result<TurnRecord> {
    Ok(TurnRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        prompt: row.get(2)?,
        screenshot_b64: row.get(3)?,
        screenshot_mime: row.get(4)?,
        answer: row.get(5)?,
        status: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn map_auth_user(row: &Row<'_>) -> rusqlite::Result<AuthUser> {
    Ok(AuthUser {
        id: row.get(0)?,
        username: row.get(1)?,
        is_admin: row.get::<_, i64>(2)? != 0,
    })
}

fn map_device(row: &Row<'_>) -> rusqlite::Result<DeviceSummary> {
    Ok(DeviceSummary {
        device_id: row.get(0)?,
        owner_user_id: row.get(1)?,
        owner_username: row.get(2)?,
    })
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(4000).collect())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn persists_sessions_and_turns() {
        let path = std::env::temp_dir().join(format!("qa-store-test-{}.db", uuid::Uuid::new_v4()));
        let store = Store::open(&path).expect("open store");
        let session = store
            .create_session("desktop-1", Some("登录问题"), Some("分析登录页"))
            .expect("create session");
        let turn = store
            .create_turn(&session.id, "分析登录页", "cG5n", "image/png")
            .expect("create turn");
        store
            .finish_turn(&turn.id, "请检查用户名", "done")
            .expect("finish turn");

        let sessions = store.list_sessions("desktop-1").expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].turn_count, 1);
        let turns = store.turns(&session.id).expect("list turns");
        assert_eq!(turns[0].answer, "请检查用户名");
        assert_eq!(turns[0].status, "done");
        let screenshot = store
            .turn_screenshot(&turn.id)
            .expect("load screenshot")
            .expect("screenshot exists");
        assert_eq!(screenshot.device_id, "desktop-1");
        assert_eq!(screenshot.screenshot_b64, "cG5n");
        assert_eq!(screenshot.screenshot_mime, "image/png");

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }

    #[test]
    fn manages_users_sessions_and_device_access() {
        let path = std::env::temp_dir().join(format!("qa-auth-test-{}.db", uuid::Uuid::new_v4()));
        let store = Store::open(&path).expect("open store");
        let admin = store
            .create_initial_admin("admin", "argon-hash-placeholder")
            .expect("create admin");
        let device_ids = ["desktop-1".to_string()];
        store
            .provision_devices(device_ids.iter(), &admin.id)
            .expect("provision device");
        let user = store
            .create_user("alice", "argon-hash-placeholder")
            .expect("create user");

        assert!(store
            .can_access_device(&admin, "desktop-1")
            .expect("admin access"));
        assert!(!store
            .can_access_device(&user, "desktop-1")
            .expect("user denied"));
        store
            .assign_device("desktop-1", &user.id)
            .expect("assign device");
        assert!(store
            .can_access_device(&user, "desktop-1")
            .expect("user access"));
        assert!(store
            .can_access_device(&admin, "desktop-1")
            .expect("admin keeps access"));

        store
            .create_auth_session("token-hash", &user.id, super::now() + 60)
            .expect("create auth session");
        assert_eq!(
            store
                .user_by_auth_session("token-hash")
                .expect("load auth session")
                .expect("authenticated user")
                .username,
            "alice"
        );
        store
            .set_user_password(&user.id, "new-hash")
            .expect("reset password");
        assert!(store
            .user_by_auth_session("token-hash")
            .expect("old login lookup")
            .is_none());

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }
}
