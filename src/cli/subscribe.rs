use crate::article_index::{ArticleIndex, LocalArticleInput};
use crate::ipc::Request;
use anyhow::{bail, Context, Result};
use serde_json::json;

use super::history::{parse_time, parse_time_end};
use super::output::{print_value, resolve};
use super::transport;

pub fn cmd_add(url: String, json_output: bool) -> Result<()> {
    let mut index = ArticleIndex::open_default()?;
    let subscription = index.add_subscription_by_url(&url)?;
    print_value(&json!(subscription), &resolve(json_output))
}

pub fn cmd_list(include_disabled: bool, json_output: bool) -> Result<()> {
    let index = ArticleIndex::open_default()?;
    let subscriptions: Vec<_> = index
        .list_subscriptions()?
        .into_iter()
        .filter(|item| include_disabled || item.enabled)
        .collect();
    print_value(&json!(subscriptions), &resolve(json_output))
}

pub fn cmd_remove(account_id: i64, json_output: bool) -> Result<()> {
    let index = ArticleIndex::open_default()?;
    if !index.remove_subscription(account_id)? {
        bail!("未找到启用中的订阅 ID: {account_id}");
    }
    print_value(
        &json!({ "id": account_id, "enabled": false }),
        &resolve(json_output),
    )
}

pub fn cmd_sync(json_output: bool) -> Result<()> {
    let mut index = ArticleIndex::open_default()?;
    if !index.list_subscriptions()?.iter().any(|item| item.enabled) {
        bail!("没有启用中的订阅，请先运行 wx subscribe add --url <公众号文章链接>");
    }
    let since = index.incremental_since()?;
    let response = transport::send(Request::BizArticles {
        limit: usize::MAX,
        account: None,
        since,
        until: None,
        unread: false,
    })?;
    let values = response
        .data
        .get("articles")
        .and_then(|value| value.as_array())
        .context("daemon 响应缺少 articles 数组")?;
    let items: Vec<LocalArticleInput> = values
        .iter()
        .filter(|value| {
            value.get("url_status").and_then(|item| item.as_str()) == Some("wechat_article")
        })
        .map(|value| serde_json::from_value(value.clone()).context("解析本地公众号文章身份失败"))
        .collect::<Result<Vec<_>>>()?;
    let summary = index.ingest_local_articles(values.len(), &items)?;
    print_value(
        &json!({
            "source": "wechat_local",
            "since_cursor": since,
            "summary": summary,
        }),
        &resolve(json_output),
    )
}

pub fn cmd_articles(
    limit: usize,
    account: Option<String>,
    since: Option<String>,
    until: Option<String>,
    json_output: bool,
) -> Result<()> {
    let since_ts = since.as_deref().map(parse_time).transpose()?;
    let until_ts = until.as_deref().map(parse_time_end).transpose()?;
    let index = ArticleIndex::open_default()?;
    let articles = index.query_articles(account.as_deref(), since_ts, until_ts, limit)?;
    print_value(
        &json!({ "count": articles.len(), "articles": articles }),
        &resolve(json_output),
    )
}
