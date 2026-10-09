//! 界面语言：启动时读一次系统 locale，之后所有后端文案按它输出中英文。
//!
//! 判定规则与前端一致：locale 以 zh 开头（zh-CN、zh-TW 等）用中文，其余（含 C
//! locale 与未识别值）一律英文——默认国际化，未覆盖语言回退到英文。CLI 不经过
//! 这里，保持仓库约定的中文文案。

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// 界面语言。进程内只判定一次。
pub fn lang() -> Lang {
    *LANG.get_or_init(|| {
        let locale = sys_locale::get_locale().unwrap_or_default();
        if locale.to_ascii_lowercase().starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    })
}

/// 按语言取两个静态候选之一（先中文后英文）。
pub fn pick<'a>(lang: Lang, zh: &'a str, en: &'a str) -> &'a str {
    match lang {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 引擎层用的语言代码（"zh" / "en"），例如回读校验的差异描述。
pub fn lang_code() -> &'static str {
    match lang() {
        Lang::Zh => "zh",
        Lang::En => "en",
    }
}
