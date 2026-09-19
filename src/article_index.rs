use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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

pub struct ArticleIndex {
    conn: Connection,
}

impl ArticleIndex {
    pub fn open_default() -> Result<Self> {
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
}
