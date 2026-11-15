use crate::config::Language;
use crate::i18n::{I18n, MessageId};

#[test]
fn zh_catalog_uses_one_word_for_compose() {
    // #3363 settled on 编写 for "compose"; 组稿 is an editorial term that
    // sat beside it in the same zh-CN /help panel (#3368). Guard the whole
    // catalog so one label cannot drift back to the other translation.
    let zh = I18n::new(Language::ZhCn);
    for id in MessageId::ALL {
        let value = zh.t(*id);
        assert!(!value.contains("组稿"), "{id:?} still uses 组稿: {value}");
    }
}
