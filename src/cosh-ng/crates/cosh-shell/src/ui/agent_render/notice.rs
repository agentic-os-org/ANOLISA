pub struct NoticePanelModel<'a> {
    pub title: &'a str,
    pub body: Vec<String>,
    pub footer: Option<&'a str>,
}
