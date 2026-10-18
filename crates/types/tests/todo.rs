use runode_types::todo::TodoSpec;

#[test]
fn instructions_list_the_skills_and_attachments_after_the_prompt() {
    let spec = TodoSpec { title: "Fix the login".into(), ..TodoSpec::default() };
    assert_eq!(spec.instructions(), "Fix the login");
    let spec = TodoSpec {
        prompt: "Read the issue.".into(),
        skills: vec!["review".into(), "tdd".into()],
        attachments: vec!["/tmp/shot.png".into(), "/src/spec.md".into()],
        ..spec
    };
    assert_eq!(
        spec.instructions(),
        "Read the issue.\n\nUse these skills: review, tdd.\n\nAttached files:\n- /tmp/shot.png\n- /src/spec.md"
    );
}
