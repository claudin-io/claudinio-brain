//! Turning what someone typed into a stable identity key.
//!
//! Getting this exactly right is the whole deduplication story: entity merging,
//! alias matching and predicate identity all fall out of it. Getting it wrong is
//! invisible at first and then permanent -- a brain quietly grows two parallel
//! histories for one thing.

use unicode_normalization::UnicodeNormalization;

/// Normalizes a label into a lookup key.
///
/// NFKC first, because "preço" typed directly (NFC) and pasted from a macOS
/// filename (NFD) are different byte sequences for the same word. Then anything
/// that is not alphanumeric becomes `_`, runs collapse, and the result is
/// case-folded. Accents are preserved -- `preço` and `preco` are different keys.
/// Folding those together is a *search* concern, handled by FTS5's
/// `remove_diacritics` in Passo 4, not an identity concern.
pub fn key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_sep = false;

    for c in s.nfkc() {
        if c.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.extend(c.to_lowercase());
        } else {
            // Underscores and separators alike collapse into a single `_`, and
            // only if something follows, so keys never start or end with one.
            pending_sep = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::key;

    #[test]
    fn separators_collapse_and_case_folds() {
        assert_eq!(key("Produto A"), "produto_a");
        assert_eq!(key("produto-a"), "produto_a");
        assert_eq!(key("produto_a"), "produto_a");
        assert_eq!(key("  Produto   A  "), "produto_a");
        assert_eq!(key("produto/a.v2"), "produto_a_v2");
    }

    #[test]
    fn composition_forms_unify_but_accents_are_kept() {
        assert_eq!(key("pre\u{e7}o"), key("prec\u{327}o"));
        assert_ne!(key("preço"), key("preco"));
    }

    #[test]
    fn non_latin_scripts_survive() {
        assert_eq!(key("日本語"), "日本語");
        assert_eq!(key("Привет мир"), "привет_мир");
    }

    #[test]
    fn degenerate_input_yields_an_empty_key() {
        assert_eq!(key(""), "");
        assert_eq!(key("---"), "");
    }
}
