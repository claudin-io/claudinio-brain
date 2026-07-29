//! The studio: a brain, drawn.
//!
//! Two surfaces over one page. `brain export` writes a single self-contained
//! HTML file -- a photograph of the brain that opens from `file://` with no
//! server and no network. `brain studio` serves that same page from localhost
//! and adds a write API behind it, so the graph becomes editable.
//!
//! They render from the identical template on purpose. A read-only viewer that
//! drifts from the editor is a viewer nobody trusts, and the fastest way to
//! guarantee they cannot drift is to have one of them.
//!
//! Everything the page needs is compiled in: three.js is a vendored classic
//! script (see `tools/vendor-three.sh` for why it is not a module), the styles
//! and the app are `include_str!`, and the data is inlined as JSON. The page
//! makes no network request, which is the same promise the binary makes.

pub mod server;
pub mod snapshot;

pub use snapshot::Snapshot;

const TEMPLATE: &str = include_str!("assets/studio.html");
const CSS: &str = include_str!("assets/studio.css");
const APP_JS: &str = include_str!("assets/studio.js");
const THREE_JS: &str = include_str!("assets/vendor/three.bundle.js");

/// Renders the whole page, data included.
pub fn render_page(snap: &Snapshot) -> Result<String, serde_json::Error> {
    let json = snap.to_inline_json()?;
    Ok(fill(
        TEMPLATE,
        &[
            ("__BRAIN_TITLE__", &escape_html(&snap.brain_label)),
            ("__BRAIN_CSS__", CSS),
            ("__BRAIN_THREE__", THREE_JS),
            ("__BRAIN_APP__", APP_JS),
            ("__BRAIN_SNAPSHOT__", &json),
        ],
    ))
}

/// Substitutes every placeholder in one left-to-right pass.
///
/// Chained `str::replace` would rescan the output, so a stored fact containing
/// the literal text `__BRAIN_APP__` would get substituted as if it were a
/// placeholder. Data is not template, and one pass is what enforces that.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out =
        String::with_capacity(template.len() + vars.iter().map(|(_, v)| v.len()).sum::<usize>());
    let mut rest = template;
    'outer: while !rest.is_empty() {
        // The earliest placeholder wins, so the scan never skips one.
        let next = vars
            .iter()
            .filter_map(|(k, v)| rest.find(k).map(|at| (at, *k, *v)))
            .min_by_key(|(at, _, _)| *at);
        match next {
            Some((at, key, value)) => {
                out.push_str(&rest[..at]);
                out.push_str(value);
                rest = &rest[at + key.len()..];
            }
            None => {
                out.push_str(rest);
                break 'outer;
            }
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_substitutes_each_placeholder_once() {
        assert_eq!(
            fill("a __X__ b __Y__", &[("__X__", "1"), ("__Y__", "2")]),
            "a 1 b 2"
        );
    }

    /// The reason `fill` exists rather than chained `replace`: a value that
    /// happens to contain another placeholder must survive as data.
    #[test]
    fn substituted_values_are_not_rescanned() {
        let out = fill("[__X__][__Y__]", &[("__X__", "__Y__"), ("__Y__", "ok")]);
        assert_eq!(out, "[__Y__][ok]");
    }

    #[test]
    fn missing_placeholder_leaves_template_intact() {
        assert_eq!(fill("nothing here", &[("__X__", "1")]), "nothing here");
    }
}
