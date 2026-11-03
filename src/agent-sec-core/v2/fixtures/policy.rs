// Shared authoring input for lifecycle tests; production accepts general rules.
fn file_policy(files: Vec<String>) -> asc_policy_types::authoring::PolicyTemplate {
    use asc_policy_types::authoring::{
        Action, Category, Comparison, Condition, Effect, PolicyTemplate, Resource, Rule, Scalar,
        Value,
    };
    PolicyTemplate {
        spec_version: "0.1".into(),
        description: None,
        rules: files
            .into_iter()
            .map(|path| Rule {
                effect: Effect::Block,
                category: Category::File,
                action: Action::Write,
                target: Resource::File { path },
                condition: Some(Condition::Operation(Comparison::Eq(Value::Scalar(
                    Scalar::String("delete".into()),
                )))),
                previous: None,
                because: None,
            })
            .collect(),
    }
}
