//! `haest models`: the catalog, grouped, with each model's window and price.

use haest::{core::catalog::ModelInfo, providers::Catalog};

fn window(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens % 1_000_000 == 0 {
        format!("{}M", tokens / 1_000_000)
    } else {
        format!("{}K", tokens / 1000)
    }
}

/// `$3 / $15` per million tokens in / out, or `-` for an unpriced model.
fn price(m: &ModelInfo) -> String {
    m.pricing
        .as_ref()
        .map_or("-".into(), |p| format!("${} / ${}", p.input, p.output))
}

pub fn render(catalog: &Catalog) -> String {
    let mut out = String::new();
    for group in catalog.groups() {
        out.push_str(&format!("{}\n", group.label));
        for m in &group.models {
            let default = if m.spec == catalog.default_model() {
                "  (default)"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {:<52} {:>5}  {}{default}\n",
                m.spec,
                window(m.context_window),
                price(m)
            ));
        }
    }
    out.push_str(
        "\nAny provider:model works, these are the ones haest knows the window and price of.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_names_every_model_and_marks_the_default() {
        let catalog = Catalog::builtin();
        let text = render(catalog);
        assert!(catalog.models().all(|m| text.contains(&m.spec)));
        let default_line = text.lines().find(|l| l.contains("(default)")).unwrap();
        assert!(default_line.contains(catalog.default_model()));
        assert_eq!(window(1_000_000), "1M");
        assert_eq!(window(200_000), "200K");
    }
}
