use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use haest_core::{catalog::ModelInfo, context::PromptInputs};
use serde_json::json;

use super::*;

fn inputs(spec: &str) -> PromptInputs {
    PromptInputs {
        cwd: "/w".into(),
        model_spec: spec.into(),
        date: "2026-11-20".into(),
        ..PromptInputs::default()
    }
}

fn render(source: &str, model: Option<&ModelInfo>) -> Result<String, TemplateError> {
    let spec = model.map_or("local:m", |m| m.spec.as_str());
    PromptTemplate::new(source)?.render(&PromptContext::new(&inputs(spec), &[], model))
}

#[test]
fn a_syntax_error_fails_when_the_template_is_made_and_names_its_line() {
    let err = PromptTemplate::new("ok\n{% if %}").unwrap_err();
    assert_eq!(err.line(), Some(2));
}

#[test]
fn an_unknown_name_is_false_in_an_if_and_an_error_when_printed() {
    assert_eq!(render("{% if nope %}x{% endif %}y", None).unwrap(), "y");
    let err = render("a\n{{ nope }}", None).unwrap_err();
    assert_eq!(err.line(), Some(2));
}

#[test]
fn a_model_the_catalog_does_not_know_has_none_for_its_facts() {
    // `none > 5` is false rather than an error, so one template serves a
    // model with no row.
    let source =
        "{% if model.generation > 5 %}new{% else %}{{ model.provider }}/{{ model.id }}{% endif %}";
    assert_eq!(render(source, None).unwrap(), "local/m");
    let row = ModelInfo::new("local:m", 1).with_generation(5.5);
    assert_eq!(render(source, Some(&row)).unwrap(), "new");
}

#[test]
fn templates_written_for_hugging_face_render() {
    // Python string methods (pycompat), loop controls, tojson, and blocks
    // that leave no blank line behind (trim_blocks, lstrip_blocks).
    let source = r#"
{% for t in ["a ", "b", "c"] %}
    {% if t == "c" %}{% break %}{% endif %}
{{ t.strip().upper() }}
{% endfor %}
{{ {"k": 1} | tojson }}
"#;
    assert_eq!(render(source, None).unwrap(), "A\nB\n{\"k\": 1}");
    let err = render(r#"{{ raise_exception("no tools") }}"#, None).unwrap_err();
    assert!(err.to_string().contains("no tools"), "{err}");
}

#[test]
fn a_name_goes_through_the_resolvers_in_order_then_the_properties_then_haest() {
    let template = PromptTemplate::new("{{ a }} {{ b }} {{ cwd }} {{ date }}")
        .unwrap()
        .with_property("b", "property")
        .with_property("date", "the host's date")
        .with_resolver(|name, next| match name {
            "a" => Some("first".into()),
            _ => next.resolve(name),
        })
        .with_resolver(|name, next| match name {
            "a" | "b" => Some("second".into()),
            "cwd" => {
                let cwd = next.resolve(name)?;
                Some(json!(format!("{}!", cwd.as_str()?)))
            }
            _ => next.resolve(name),
        });
    let out = template
        .render(&PromptContext::new(&inputs("local:m"), &[], None))
        .unwrap();
    assert_eq!(out, "first second /w! the host's date");
}

#[test]
fn a_resolver_runs_once_per_name_per_render() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let template = PromptTemplate::new("{{ user }}{{ user }}{% if user %}!{% endif %}")
        .unwrap()
        .with_resolver(move |name, next| {
            if name != "user" {
                return next.resolve(name);
            }
            counted.fetch_add(1, Ordering::Relaxed);
            Some("ada".into())
        });
    let context = PromptContext::new(&inputs("local:m"), &[], None);
    assert_eq!(template.render(&context).unwrap(), "adaada!");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    template.render(&context).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2, "a new render asks again");
}

#[test]
fn the_sections_are_haests_own_prompt() {
    let inputs = inputs("local:m");
    let context = PromptContext::new(&inputs, &[], None);
    let out = PromptTemplate::new("{{ sections.identity }}\n\n{{ sections.environment }}")
        .unwrap()
        .render(&context)
        .unwrap();
    let built = context.sections();
    assert_eq!(out, format!("{}\n\n{}", built.identity, built.environment));
}
