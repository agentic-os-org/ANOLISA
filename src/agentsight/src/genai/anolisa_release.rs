//! Anolisa OS release detection
//!
//! 通过检查 `/etc/anolisa-release` 判断当前主机是否运行 Anolisa OS，
//! 并解析其中的 `PRODUCT_TYPE=value`，用于 SLS 日志注入额外标签：
//! - `agentsight.source = "agenticos"`（仅当文件存在时写入）
//! - `agentsight.product_type = <PRODUCT_TYPE value>`（仅当 key 存在时写入）
//!
//! 解析结果通过 `OnceLock` 缓存，文件每个进程生命周期只读一次。

use std::sync::OnceLock;

/// Anolisa release 文件路径
const ANOLISA_RELEASE_PATH: &str = "/etc/anolisa-release";

/// SLS 注入字段值：`agentsight.source = "agenticos"`
pub const AGENTSIGHT_SOURCE_AGENTICOS: &str = "agenticos";

/// 解析后的 release 信息
#[derive(Debug, Clone)]
pub struct AnolisaRelease {
    /// `PRODUCT_TYPE` 字段值（key 缺失时为 None）
    pub product_type: Option<String>,
}

/// 全局缓存：`Some(_)` 表示 `/etc/anolisa-release` 存在；`None` 表示不存在
static ANOLISA_RELEASE: OnceLock<Option<AnolisaRelease>> = OnceLock::new();

/// 获取 release 信息（带缓存）。文件不存在时返回 `None`。
pub fn get() -> Option<&'static AnolisaRelease> {
    ANOLISA_RELEASE
        .get_or_init(|| match std::fs::read_to_string(ANOLISA_RELEASE_PATH) {
            Ok(content) => {
                let parsed = parse(&content);
                log::info!(
                    "Detected Anolisa OS via {}, PRODUCT_TYPE={:?}",
                    ANOLISA_RELEASE_PATH,
                    parsed.product_type
                );
                Some(parsed)
            }
            Err(_) => None,
        })
        .as_ref()
}

/// 当前主机是否运行 Anolisa OS（即 `/etc/anolisa-release` 是否存在）
pub fn is_anolisa() -> bool {
    get().is_some()
}

/// 获取 `PRODUCT_TYPE` 值（带缓存），文件不存在或 key 缺失时返回 `None`
pub fn product_type() -> Option<&'static str> {
    get().and_then(|r| r.product_type.as_deref())
}

/// 解析 shell-style key=value 文件内容（支持引号包裹与 `#` 注释）
fn parse(content: &str) -> AnolisaRelease {
    let mut product_type = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            // 去除值两侧可能的引号
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            if k == "PRODUCT_TYPE" {
                product_type = Some(v.to_string());
            }
        }
    }
    AnolisaRelease { product_type }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quoted_product_type() {
        let r = parse("PRODUCT_TYPE=\"AgenticOS\"\nFOO=bar\n");
        assert_eq!(r.product_type.as_deref(), Some("AgenticOS"));
    }

    #[test]
    fn parse_unquoted_product_type() {
        let r = parse("PRODUCT_TYPE=anolisa-server\n");
        assert_eq!(r.product_type.as_deref(), Some("anolisa-server"));
    }

    #[test]
    fn parse_skips_comments_and_blank_lines() {
        let content = "# header comment\n\nPRODUCT_TYPE='edge'\n# trailing\n";
        let r = parse(content);
        assert_eq!(r.product_type.as_deref(), Some("edge"));
    }

    #[test]
    fn parse_missing_product_type_returns_none() {
        let r = parse("FOO=bar\nBAZ=qux\n");
        assert!(r.product_type.is_none());
    }
}
