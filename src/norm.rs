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

/// Folds text the way *search* compares it, rather than the way identity does.
///
/// NFD, drop the combining marks, lowercase. The result is what
/// [`crate::brain::Brain::find`] compares against, and it is chosen to agree with
/// the FTS5 index rather than to be independently reasonable: `fact_fts` is
/// tokenized `unicode61 remove_diacritics 2`, so a term query already ignores
/// accents and case. A fragment scan that did not would make one command answer
/// the same question two ways depending on which stage found the row.
///
/// The mirror image of [`key`], and the pair is the whole rule this project
/// repeats: identity is exact, search is forgiving. `key` keeps accents so that
/// `preço` and `preco` stay two things; this drops them so that either spelling
/// finds either one.
///
/// Decomposition is what makes dropping marks correct. `é` as a single code point
/// has no combining mark to remove, so NFC input would survive untouched and the
/// fold would depend on which normalization form the text happened to arrive in.
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{fold, key};

    #[test]
    fn folding_ignores_case_and_accents() {
        assert_eq!(fold("Preço"), "preco");
        assert_eq!(fold("PREÇO"), fold("preco"));
        assert_eq!(fold("André"), "andre");
    }

    #[test]
    fn folding_agrees_across_composition_forms() {
        assert_eq!(fold("pre\u{e7}o"), fold("prec\u{327}o"));
    }

    #[test]
    fn folding_keeps_everything_else() {
        // Unlike `key`, punctuation survives: a fragment search for `_normalize`
        // or `k2.6` is exactly the case this exists for.
        assert_eq!(fold("_normalize_email"), "_normalize_email");
        assert_eq!(fold("Kimi K2.6"), "kimi k2.6");
        assert_eq!(fold("日本語"), "日本語");
    }

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
