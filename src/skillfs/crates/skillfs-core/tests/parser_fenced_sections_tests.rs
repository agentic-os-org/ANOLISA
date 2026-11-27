//! Fenced code blocks must not feed `## Parameters` / `## Returns`
//! extraction. A SKILL.md that documents the format (or any skill that
//! shows an example) contains `## Parameters` inside a fence; the parser
//! must treat it as an example, not as a contract section.

use skillfs_core::parser::parse_skill_md;

/// A fenced code block is an example, not contract text: a
/// `## Parameters` section written INSIDE a fence must not feed
/// structured extraction. Documents that teach the SKILL.md format
/// contain exactly such examples.
#[test]
fn test_parse_fenced_example_does_not_declare_parameters() {
    let content = r#"---
name: skill-authoring
description: How to author a SKILL.md
---

# Authoring

Write parameters like this:

```markdown
## Parameters

- `query` (string, required): The search query
```

No real parameter section here.
"#;

    let entry = parse_skill_md(content, "skill-authoring");

    assert!(
        entry.parameters.is_empty(),
        "a fenced example must not declare parameters, got {:?}",
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        entry.parse_status.is_ok(),
        "fenced examples must not degrade the skill, got {:?}",
        entry.parse_status
    );
}

/// The same document with a real `## Parameters` section after the
/// fenced example: only the real section's entry is extracted, and the
/// example heading must not be seen as a duplicate contract heading.
#[test]
fn test_parse_fenced_example_does_not_collide_with_the_real_section() {
    let content = r#"---
name: skill-authoring
description: How to author a SKILL.md
---

# Authoring

```markdown
## Parameters

- `example` (string, required): From the example
```

## Parameters

- `real` (string, required): The real parameter
"#;

    let entry = parse_skill_md(content, "skill-authoring");

    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["real"],
        "only the real section's parameters may be extracted"
    );
    assert!(
        entry.parse_status.is_ok(),
        "the example heading must not be a duplicate contract heading, got {:?}",
        entry.parse_status
    );
}

/// A parameter-shaped bullet inside a real section's fenced example is
/// still an example, not a declaration.
#[test]
fn test_parse_fenced_example_inside_a_real_section_is_not_extracted() {
    let content = r#"---
name: demo
description: Demo
---

## Parameters

- `real` (string, required): The real parameter

```
- `example` (string, required): From the example
```
"#;

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["real"]
    );
    assert!(
        entry.parse_status.is_ok(),
        "the fenced example must not be flagged malformed, got {:?}",
        entry.parse_status
    );
}

/// A closing fence indented by four or more spaces does not close the
/// block (CommonMark allows at most three spaces): the example stays open,
/// so the parameter-shaped content after it is still fence content.
#[test]
fn test_parse_deeply_indented_closing_fence_does_not_close() {
    let content = r#"---
name: skill-authoring
description: How to author a SKILL.md
---

# Authoring

```markdown
    ```
## Parameters

- `ghost` (string, required): example
```
"#;

    let entry = parse_skill_md(content, "skill-authoring");

    assert!(
        entry.parameters.is_empty(),
        "content past a four-space-indented closing fence is still inside the fence, got {:?}",
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        entry.parse_status.is_ok(),
        "the unterminated example must not degrade the skill, got {:?}",
        entry.parse_status
    );
}

/// A backtick fence whose info string contains a backtick is inline code,
/// not an opening fence (CommonMark): the document continues normally, so
/// the real `## Parameters` section must survive.
#[test]
fn test_parse_backtick_info_string_with_backtick_is_not_a_fence() {
    let content = r#"---
name: real-section
description: Demo
---

```a ` inline code```

## Parameters

- `real` (string, required): real
"#;

    let entry = parse_skill_md(content, "real-section");

    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["real"],
        "the real section must not be swallowed by a pseudo-fence"
    );
    assert!(
        entry.parse_status.is_ok(),
        "inline code must not degrade the skill, got {:?}",
        entry.parse_status
    );
}

/// The info-string restriction is backtick-specific (CommonMark): a tilde
/// fence may contain backticks in its info string and still opens a block,
/// keeping the example out of the contract.
#[test]
fn test_parse_tilde_fence_allows_backticks_in_info_string() {
    let content = r#"---
name: skill-authoring
description: How to author a SKILL.md
---

# Authoring

~~~markdown with `backticks`
## Parameters

- `example` (string, required): From the example
~~~
"#;

    let entry = parse_skill_md(content, "skill-authoring");

    assert!(
        entry.parameters.is_empty(),
        "a tilde-fenced example must not declare parameters, got {:?}",
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        entry.parse_status.is_ok(),
        "the tilde-fenced example must not degrade the skill, got {:?}",
        entry.parse_status
    );
}

#[test]
fn test_top_level_sections_follow_unclosed_list_fences() {
    let content = r#"---
name: demo
description: List examples followed by contract sections
---

10. Parameter example
    ```sh
    echo example

## Parameters

- `target` (string, required): Output target

10. Return example
    ~~~sh
    echo result

## Returns

- `result` (string): Output result
"#;

    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["target"],
    );
    assert_eq!(
        entry
            .returns
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["result"],
    );
}

#[test]
fn test_unclosed_nested_list_fence_ends_at_parent_item_content() {
    let content = r#"---
name: demo
description: A parameter with a nested example
---

## Parameters

- `source` (string, required): Input source
  1. Example
     ```markdown

     - `example` (string, required): Still in the example
  - `nested` (string, optional): Outside the example
- `target` (string, required): Output target
"#;

    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["source", "nested", "target"],
    );
}

#[test]
fn test_dedented_fence_opens_outside_the_list() {
    let content = r#"---
name: demo
description: Adjacent list and top-level examples
---

10. A list example
    ```markdown
    Literal text
```
## Parameters

- `example` (string, required): Still inside the top-level example
```
## Parameters

- `target` (string, required): Output target
"#;

    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(
        entry
            .parameters
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["target"],
    );
}

#[test]
fn test_fence_starting_a_list_item_hides_example_section() {
    for (opener, indent, closer) in [("- ```markdown", "  ", "```"), ("1. ~~~yaml", "   ", "~~~")] {
        let content = format!(
            "---\nname: demo\ndescription: Demo\n---\n\n{opener}\n{indent}## Parameters\n{indent}- `sample` (bogus, required): example\n{indent}{closer}\n"
        );
        let entry = parse_skill_md(&content, "demo");
        assert!(
            entry.parse_status.is_ok(),
            "{opener}: {:?}",
            entry.parse_status
        );
        assert!(
            entry.parameters.is_empty(),
            "{opener}: {:?}",
            entry.parameters
        );
    }
}

#[test]
fn test_fence_starting_a_nested_item_ends_at_indented_closer() {
    let content = r#"---
name: demo
description: Demo
---

## Parameters

- - ```markdown
    ## Parameters
    - `sample` (bogus, required): example
    ```
    - `real` (string, required): After the closer, still in the item
"#;
    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(entry.parameters.len(), 1);
    assert_eq!(entry.parameters[0].name, "real");
}

#[test]
fn test_unclosed_fence_starting_a_list_item_ends_at_dedent() {
    let content = r#"---
name: demo
description: Demo
---

- ```markdown
  ## Parameters
  - `sample` (bogus, required): example
## Parameters
- `real` (string, required): After the item
"#;
    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(entry.parameters.len(), 1);
    assert_eq!(entry.parameters[0].name, "real");
}

#[test]
fn test_indented_code_starting_a_list_item_does_not_open_a_fence() {
    let content = r#"---
name: demo
description: Demo
---

-     ```markdown

  ## Parameters
  - `real` (string, required): At the item content column
"#;
    let entry = parse_skill_md(content, "demo");
    assert!(entry.parse_status.is_ok(), "{:?}", entry.parse_status);
    assert_eq!(entry.parameters.len(), 1);
    assert_eq!(entry.parameters[0].name, "real");
}
