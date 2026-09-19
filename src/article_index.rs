use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const LOCAL_SOURCE: &str = "wechat_local";
const INDEX_PATH_ENV: &str = "WX_ARTICLE_INDEX_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArticleUrlIdentity {
    Wechat {
        biz: String,
        mid: String,
        idx: String,
        sn: Option<String>,
        canonical_url: String,
        identity_key: String,
    },
    External {
        normalized_url: String,
        normalized_url_hash: String,
    },
    Invalid,
}

pub fn parse_article_url(raw: &str) -> ArticleUrlIdentity {
    let cleaned = raw.trim().replace("&amp;", "&");
    if cleaned.is_empty() || cleaned.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return ArticleUrlIdentity::Invalid;
    }

    let Some(scheme_end) = cleaned.find("://") else {
        return ArticleUrlIdentity::Invalid;
    };
    let scheme = cleaned[..scheme_end].to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return ArticleUrlIdentity::Invalid;
    }

    let after_scheme = &cleaned[scheme_end + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return ArticleUrlIdentity::Invalid;
    }
    let host = authority
        .split_once(':')
        .map(|(host, _)| host)
        .unwrap_or(authority)
        .to_ascii_lowercase();
    if host.is_empty() {
        return ArticleUrlIdentity::Invalid;
    }

    let suffix = &after_scheme[authority_end..];
    let without_fragment = suffix.split('#').next().unwrap_or_default();
    if host == "mp.weixin.qq.com" {
        let query = without_fragment
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_default();
        let param = |name: &str| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(key).as_deref() == Some(name))
                    .then(|| percent_decode(value))
                    .flatten()
            })
        };

        let (Some(biz), Some(mid), Some(idx)) = (param("__biz"), param("mid"), param("idx")) else {
            return ArticleUrlIdentity::Invalid;
        };
        if biz.is_empty()
            || mid.is_empty()
            || idx.is_empty()
            || !mid.bytes().all(|b| b.is_ascii_digit())
            || !idx.bytes().all(|b| b.is_ascii_digit())
        {
            return ArticleUrlIdentity::Invalid;
        }
        let sn = param("sn").filter(|value| !value.is_empty());
        let mut canonical_url = format!(
            "https://mp.weixin.qq.com/s?__biz={}&mid={}&idx={}",
            percent_encode(&biz),
            percent_encode(&mid),
            percent_encode(&idx)
        );
        if let Some(value) = &sn {
            canonical_url.push_str("&sn=");
            canonical_url.push_str(&percent_encode(value));
        }
        let identity_key = format!("wechat:{biz}:{mid}:{idx}");
        return ArticleUrlIdentity::Wechat {
            biz,
            mid,
            idx,
            sn,
            canonical_url,
            identity_key,
        };
    }

    let normalized_url = format!(
        "{scheme}://{}{without_fragment}",
        authority.to_ascii_lowercase()
    );
    let normalized_url_hash = format!("{:x}", Sha256::digest(normalized_url.as_bytes()));
    ArticleUrlIdentity::External {
        normalized_url,
        normalized_url_hash,
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SubscriptionRecord {
    pub id: i64,
    pub account_id: i64,
    pub wechat_biz: String,
    pub seed_url: String,
    pub status: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LocalArticleInput {
    pub account_username: String,
    #[serde(rename = "account")]
    pub account_display: String,
    pub wechat_biz: String,
    pub mid: String,
    pub idx: String,
    #[serde(default)]
    pub sn: Option<String>,
    pub canonical_url: String,
    pub title: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub cover_url: String,
    #[serde(rename = "timestamp")]
    pub publish_time: i64,
    pub recv_time: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexedArticle {
    pub id: i64,
    pub account_id: i64,
    pub account_username: String,
    pub account: String,
    pub wechat_biz: String,
    pub mid: String,
    pub idx: String,
    pub sn: Option<String>,
    pub canonical_url: String,
    pub title: String,
    pub digest: String,
    pub cover_url: String,
    pub publish_time: i64,
    pub recv_time: i64,
    pub source: String,
    pub content_status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncSummary {
    pub run_id: i64,
    pub scanned: usize,
    pub matched: usize,
    pub inserted: usize,
    pub updated: usize,
    pub skipped: usize,
    pub cursors_advanced: usize,
}

pub struct ArticleIndex {
    conn: Connection,
}

impl ArticleIndex {
    pub fn open_default() -> Result<Self> {
        if let Some(path) = std::env::var_os(INDEX_PATH_ENV).filter(|value| !value.is_empty()) {
            return Self::open(PathBuf::from(path));
        }
        let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::open(base.join(".wx-cli").join("index").join("articles.db"))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建文章索引目录失败: {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("打开文章索引失败: {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS accounts (
                 id INTEGER PRIMARY KEY,
                 account_username TEXT UNIQUE,
                 wechat_biz TEXT NOT NULL UNIQUE,
                 display_name TEXT,
                 source TEXT NOT NULL CHECK(source = 'wechat_local'),
                 first_seen_at INTEGER NOT NULL,
                 last_seen_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS account_aliases (
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 alias TEXT NOT NULL,
                 first_seen_at INTEGER NOT NULL,
                 last_seen_at INTEGER NOT NULL,
                 UNIQUE(account_id, alias)
             );
             CREATE TABLE IF NOT EXISTS subscriptions (
                 id INTEGER PRIMARY KEY,
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 wechat_biz TEXT NOT NULL UNIQUE,
                 seed_url TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('pending_local_discovery', 'active')),
                 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS articles (
                 id INTEGER PRIMARY KEY,
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 wechat_biz TEXT NOT NULL,
                 mid TEXT,
                 idx TEXT,
                 sn TEXT,
                 canonical_url TEXT NOT NULL,
                 normalized_url_hash TEXT,
                 title TEXT NOT NULL,
                 digest TEXT NOT NULL DEFAULT '',
                 cover_url TEXT NOT NULL DEFAULT '',
                 publish_time INTEGER NOT NULL,
                 recv_time INTEGER NOT NULL,
                 discovered_at INTEGER NOT NULL,
                 source TEXT NOT NULL CHECK(source = 'wechat_local'),
                 content_status TEXT NOT NULL DEFAULT 'metadata_only',
                 UNIQUE(wechat_biz, mid, idx),
                 UNIQUE(normalized_url_hash)
             );
             CREATE TABLE IF NOT EXISTS sync_cursors (
                 source TEXT NOT NULL CHECK(source = 'wechat_local'),
                 shard_key TEXT NOT NULL,
                 last_recv_time INTEGER NOT NULL DEFAULT 0,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY(source, shard_key)
             );
             CREATE TABLE IF NOT EXISTS sync_runs (
                 id INTEGER PRIMARY KEY,
                 source TEXT NOT NULL CHECK(source = 'wechat_local'),
                 started_at INTEGER NOT NULL,
                 finished_at INTEGER,
                 status TEXT NOT NULL,
                 scanned INTEGER NOT NULL DEFAULT 0,
                 inserted INTEGER NOT NULL DEFAULT 0,
                 updated INTEGER NOT NULL DEFAULT 0,
                 error TEXT
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn add_subscription_by_url(&mut self, raw_url: &str) -> Result<SubscriptionRecord> {
        let ArticleUrlIdentity::Wechat {
            biz, canonical_url, ..
        } = parse_article_url(raw_url)
        else {
            bail!("订阅链接必须是包含 __biz、mid、idx 的微信公众号文章链接");
        };

        let now = Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO accounts (wechat_biz, source, first_seen_at, last_seen_at)
             VALUES (?1, 'wechat_local', ?2, ?2)
             ON CONFLICT(wechat_biz) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![biz, now],
        )?;
        let account_id: i64 = tx.query_row(
            "SELECT id FROM accounts WHERE wechat_biz = ?1",
            params![biz],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO subscriptions
                 (account_id, wechat_biz, seed_url, status, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending_local_discovery', 1, ?4, ?4)
             ON CONFLICT(wechat_biz) DO UPDATE SET
                 seed_url = excluded.seed_url, enabled = 1, updated_at = excluded.updated_at",
            params![account_id, biz, canonical_url, now],
        )?;
        let record = query_subscription(&tx, &biz)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, wechat_biz, seed_url, status, enabled, created_at, updated_at
             FROM subscriptions ORDER BY created_at, id",
        )?;
        Ok(stmt
            .query_map([], subscription_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn remove_subscription(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE subscriptions SET enabled = 0, updated_at = ?1 WHERE id = ?2 AND enabled = 1",
            params![Utc::now().timestamp(), id],
        )? > 0)
    }

    pub fn incremental_since(&self) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.wechat_biz, COALESCE(c.last_recv_time, 0)
             FROM subscriptions s
             LEFT JOIN sync_cursors c
               ON c.source = 'wechat_local' AND c.shard_key = s.wechat_biz
             WHERE s.enabled = 1",
        )?;
        let cursors: Vec<i64> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<std::result::Result<_, _>>()?;
        if cursors.is_empty() || cursors.iter().any(|value| *value == 0) {
            Ok(None)
        } else {
            Ok(cursors.into_iter().min())
        }
    }

    pub fn ingest_local_articles(
        &mut self,
        scanned: usize,
        items: &[LocalArticleInput],
    ) -> Result<SyncSummary> {
        let now = Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO sync_runs (source, started_at, status, scanned)
             VALUES ('wechat_local', ?1, 'running', ?2)",
            params![now, scanned as i64],
        )?;
        let run_id = tx.last_insert_rowid();

        let enabled: HashSet<String> = {
            let mut stmt = tx.prepare("SELECT wechat_biz FROM subscriptions WHERE enabled = 1")?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        let mut cursors: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT shard_key, last_recv_time FROM sync_cursors WHERE source = 'wechat_local'",
            )?;
            for row in stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
                let (biz, cursor) = row?;
                cursors.insert(biz, cursor);
            }
        }

        let mut inserted = 0usize;
        let mut updated = 0usize;
        let mut matched = 0usize;
        let mut max_recv_by_biz: HashMap<String, i64> = HashMap::new();

        for item in items {
            if !enabled.contains(&item.wechat_biz) {
                continue;
            }
            matched += 1;
            max_recv_by_biz
                .entry(item.wechat_biz.clone())
                .and_modify(|value| *value = (*value).max(item.recv_time))
                .or_insert(item.recv_time);
            if item.recv_time < cursors.get(&item.wechat_biz).copied().unwrap_or(0) {
                continue;
            }

            tx.execute(
                "UPDATE accounts SET account_username = ?1, display_name = ?2, last_seen_at = ?3
                 WHERE wechat_biz = ?4",
                params![
                    item.account_username,
                    item.account_display,
                    now,
                    item.wechat_biz
                ],
            )?;
            let account_id: i64 = tx.query_row(
                "SELECT id FROM accounts WHERE wechat_biz = ?1",
                params![item.wechat_biz],
                |row| row.get(0),
            )?;
            if !item.account_display.is_empty() {
                tx.execute(
                    "INSERT INTO account_aliases (account_id, alias, first_seen_at, last_seen_at)
                     VALUES (?1, ?2, ?3, ?3)
                     ON CONFLICT(account_id, alias) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                    params![account_id, item.account_display, now],
                )?;
            }
            tx.execute(
                "UPDATE subscriptions SET status = 'active', updated_at = ?1 WHERE wechat_biz = ?2",
                params![now, item.wechat_biz],
            )?;

            let existed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE wechat_biz = ?1 AND mid = ?2 AND idx = ?3)",
                params![item.wechat_biz, item.mid, item.idx],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO articles
                   (account_id, wechat_biz, mid, idx, sn, canonical_url, title, digest, cover_url,
                    publish_time, recv_time, discovered_at, source, content_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'wechat_local', 'metadata_only')
                 ON CONFLICT(wechat_biz, mid, idx) DO UPDATE SET
                   account_id = excluded.account_id, sn = excluded.sn,
                   canonical_url = excluded.canonical_url, title = excluded.title,
                   digest = excluded.digest, cover_url = excluded.cover_url,
                   publish_time = excluded.publish_time, recv_time = excluded.recv_time",
                params![
                    account_id, item.wechat_biz, item.mid, item.idx, item.sn,
                    item.canonical_url, item.title, item.digest, item.cover_url,
                    item.publish_time, item.recv_time, now
                ],
            )?;
            if existed {
                updated += 1;
            } else {
                inserted += 1;
            }
        }

        for (biz, recv_time) in &max_recv_by_biz {
            tx.execute(
                "INSERT INTO sync_cursors (source, shard_key, last_recv_time, updated_at)
                 VALUES ('wechat_local', ?1, ?2, ?3)
                 ON CONFLICT(source, shard_key) DO UPDATE SET
                   last_recv_time = MAX(last_recv_time, excluded.last_recv_time),
                   updated_at = excluded.updated_at",
                params![biz, recv_time, now],
            )?;
        }
        let skipped = scanned.saturating_sub(matched);
        tx.execute(
            "UPDATE sync_runs SET finished_at = ?1, status = 'completed', inserted = ?2, updated = ?3
             WHERE id = ?4",
            params![now, inserted as i64, updated as i64, run_id],
        )?;
        tx.commit()?;
        Ok(SyncSummary {
            run_id,
            scanned,
            matched,
            inserted,
            updated,
            skipped,
            cursors_advanced: max_recv_by_biz.len(),
        })
    }

    pub fn query_articles(
        &self,
        account: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
        limit: usize,
    ) -> Result<Vec<IndexedArticle>> {
        let account_pattern = account.map(|value| format!("%{}%", value.to_lowercase()));
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.account_id, COALESCE(ac.account_username, ''),
                    COALESCE(ac.display_name, ''), a.wechat_biz, a.mid, a.idx, a.sn,
                    a.canonical_url, a.title, a.digest, a.cover_url, a.publish_time, a.recv_time,
                    a.source, a.content_status
             FROM articles a JOIN accounts ac ON ac.id = a.account_id
             WHERE (?1 IS NULL OR lower(COALESCE(ac.display_name, '')) LIKE ?1
                              OR lower(COALESCE(ac.account_username, '')) LIKE ?1
                              OR lower(a.wechat_biz) LIKE ?1)
               AND (?2 IS NULL OR a.publish_time >= ?2)
               AND (?3 IS NULL OR a.publish_time <= ?3)
             ORDER BY a.publish_time DESC, a.id DESC LIMIT ?4",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        Ok(stmt
            .query_map(params![account_pattern, since, until, limit], |row| {
                Ok(IndexedArticle {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    account_username: row.get(2)?,
                    account: row.get(3)?,
                    wechat_biz: row.get(4)?,
                    mid: row.get(5)?,
                    idx: row.get(6)?,
                    sn: row.get(7)?,
                    canonical_url: row.get(8)?,
                    title: row.get(9)?,
                    digest: row.get(10)?,
                    cover_url: row.get(11)?,
                    publish_time: row.get(12)?,
                    recv_time: row.get(13)?,
                    source: row.get(14)?,
                    content_status: row.get(15)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

fn query_subscription(conn: &Connection, biz: &str) -> rusqlite::Result<SubscriptionRecord> {
    conn.query_row(
        "SELECT id, account_id, wechat_biz, seed_url, status, enabled, created_at, updated_at
         FROM subscriptions WHERE wechat_biz = ?1",
        params![biz],
        subscription_from_row,
    )
}

fn subscription_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubscriptionRecord> {
    Ok(SubscriptionRecord {
        id: row.get(0)?,
        account_id: row.get(1)?,
        wechat_biz: row.get(2)?,
        seed_url: row.get(3)?,
        status: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wechat_article_identity_from_html_escaped_url() {
        let parsed = parse_article_url(
            "https://mp.weixin.qq.com/s?__biz=MzI1MA==&amp;mid=2247484000&amp;idx=2&amp;sn=abc123#rd",
        );

        assert_eq!(
            parsed,
            ArticleUrlIdentity::Wechat {
                biz: "MzI1MA==".into(),
                mid: "2247484000".into(),
                idx: "2".into(),
                sn: Some("abc123".into()),
                canonical_url:
                    "https://mp.weixin.qq.com/s?__biz=MzI1MA%3D%3D&mid=2247484000&idx=2&sn=abc123"
                        .into(),
                identity_key: "wechat:MzI1MA==:2247484000:2".into(),
            }
        );
    }

    #[test]
    fn identity_ignores_sn_and_tracking_parameters() {
        let first = parse_article_url(
            "http://mp.weixin.qq.com/s?__biz=MzA%3D&mid=42&idx=1&sn=old&scene=21",
        );
        let second =
            parse_article_url("https://mp.weixin.qq.com/s?idx=1&mid=42&__biz=MzA%3D&sn=new");

        let key = |value: ArticleUrlIdentity| match value {
            ArticleUrlIdentity::Wechat { identity_key, .. } => identity_key,
            other => panic!("expected WeChat identity, got {other:?}"),
        };
        assert_eq!(key(first), key(second));
    }

    #[test]
    fn falls_back_to_normalized_hash_for_external_article() {
        let parsed = parse_article_url("HTTPS://Example.COM/news/1?b=2#section");
        match parsed {
            ArticleUrlIdentity::External {
                normalized_url,
                normalized_url_hash,
            } => {
                assert_eq!(normalized_url, "https://example.com/news/1?b=2");
                assert_eq!(normalized_url_hash.len(), 64);
            }
            other => panic!("expected external identity, got {other:?}"),
        }
    }

    #[test]
    fn rejects_incomplete_wechat_and_non_http_urls() {
        assert_eq!(
            parse_article_url("https://mp.weixin.qq.com/s?__biz=MzA%3D&mid=42"),
            ArticleUrlIdentity::Invalid
        );
        assert_eq!(
            parse_article_url("javascript:alert(1)"),
            ArticleUrlIdentity::Invalid
        );
        assert_eq!(parse_article_url("not a url"), ArticleUrlIdentity::Invalid);
    }

    #[test]
    fn url_subscription_is_idempotent_and_can_be_disabled() {
        let path = std::env::temp_dir().join(format!(
            "wx-cli-article-index-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut index = ArticleIndex::open(&path).expect("open test index");
        let first = index
            .add_subscription_by_url("https://mp.weixin.qq.com/s?__biz=MzA%3D&mid=42&idx=1&sn=old")
            .expect("add first subscription");
        let second = index
            .add_subscription_by_url("https://mp.weixin.qq.com/s?idx=1&mid=42&__biz=MzA%3D&sn=new")
            .expect("add same account again");

        assert_eq!(first.id, second.id);
        assert_eq!(index.list_subscriptions().unwrap().len(), 1);
        assert_eq!(second.status, "pending_local_discovery");
        assert!(index.remove_subscription(second.id).unwrap());
        assert!(!index.list_subscriptions().unwrap()[0].enabled);

        drop(index);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn subscription_rejects_external_url_before_database_write() {
        let path = std::env::temp_dir().join(format!(
            "wx-cli-article-index-external-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut index = ArticleIndex::open(&path).unwrap();
        let error = index
            .add_subscription_by_url("https://example.com/article")
            .unwrap_err();
        assert!(error.to_string().contains("微信公众号文章链接"));
        assert!(index.list_subscriptions().unwrap().is_empty());
        drop(index);
        let _ = std::fs::remove_file(path);
    }

    fn local_article(biz: &str, mid: &str, recv_time: i64, account: &str) -> LocalArticleInput {
        LocalArticleInput {
            account_username: format!("gh_{biz}"),
            account_display: account.into(),
            wechat_biz: biz.into(),
            mid: mid.into(),
            idx: "1".into(),
            sn: Some("sn".into()),
            canonical_url: format!("https://mp.weixin.qq.com/s?__biz={biz}&mid={mid}&idx=1&sn=sn"),
            title: format!("article-{mid}"),
            digest: "digest".into(),
            cover_url: "https://example.com/cover.jpg".into(),
            publish_time: recv_time - 10,
            recv_time,
        }
    }

    #[test]
    fn sync_is_subscription_scoped_idempotent_and_cursor_based() {
        let path = std::env::temp_dir().join(format!(
            "wx-cli-article-sync-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut index = ArticleIndex::open(&path).unwrap();
        index
            .add_subscription_by_url("https://mp.weixin.qq.com/s?__biz=BizA&mid=1&idx=1")
            .unwrap();
        assert_eq!(index.incremental_since().unwrap(), None);

        let first = vec![
            local_article("BizA", "10", 100, "Alpha"),
            local_article("BizB", "20", 110, "Other"),
        ];
        let summary = index.ingest_local_articles(first.len(), &first).unwrap();
        assert_eq!(summary.inserted, 1);
        assert_eq!(summary.matched, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(index.incremental_since().unwrap(), Some(100));

        let second = vec![
            local_article("BizA", "10", 100, "Alpha renamed"),
            local_article("BizA", "11", 120, "Alpha renamed"),
        ];
        let summary = index.ingest_local_articles(second.len(), &second).unwrap();
        assert_eq!(summary.inserted, 1);
        assert_eq!(summary.updated, 1);
        assert_eq!(index.incremental_since().unwrap(), Some(120));
        let rows = index
            .query_articles(Some("renamed"), None, None, 10)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].mid, "11");
        assert_eq!(rows[0].source, LOCAL_SOURCE);

        let alias_count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM account_aliases WHERE alias IN ('Alpha', 'Alpha renamed')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_count, 2);
        drop(index);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn adding_new_subscription_forces_one_full_backfill() {
        let path = std::env::temp_dir().join(format!(
            "wx-cli-article-new-sub-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut index = ArticleIndex::open(&path).unwrap();
        index
            .add_subscription_by_url("https://mp.weixin.qq.com/s?__biz=BizA&mid=1&idx=1")
            .unwrap();
        index
            .ingest_local_articles(1, &[local_article("BizA", "10", 100, "Alpha")])
            .unwrap();
        assert_eq!(index.incremental_since().unwrap(), Some(100));

        index
            .add_subscription_by_url("https://mp.weixin.qq.com/s?__biz=BizB&mid=2&idx=1")
            .unwrap();
        assert_eq!(index.incremental_since().unwrap(), None);
        drop(index);
        let _ = std::fs::remove_file(path);
    }
}
