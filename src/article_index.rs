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
}
use sha2::{Digest, Sha256};
