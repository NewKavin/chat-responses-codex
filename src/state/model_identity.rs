//! Canonical model identity: the single source of truth for "which model
//! spellings are the same model".
//!
//! Upstreams name the same model with different casing (`GLM-4.5` vs
//! `glm-4.5`, `DeepSeek-V3` vs `deepseek-v3`), and the gateway stores those
//! spellings verbatim because the upstream may be case-sensitive on the wire.
//! All *comparison and dedup* paths therefore route through this module
//! (`canonical_model_id`), while *stored spelling* is never rewritten.
//!
//! The matching switch `model_case_insensitive_matching` (runtime setting,
//! default true) selects between canonical folding and the legacy exact
//! comparison; when disabled every helper degrades to exact matching so a
//! deployment with genuinely case-distinct models keeps its current behavior.

/// Canonical form used as the comparison/dedup key: trimmed, ASCII-lowercased.
/// Mirrors the `normalize_model_name` precedent in `state/usage.rs`.
pub fn canonical_model_id(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

/// Canonical equality: `true` iff both models fold to the same trimmed
/// lowercase key. Empty/whitespace-only input never compares equal to itself.
pub fn models_equivalent(a: &str, b: &str) -> bool {
    let a = a.trim();
    let b = b.trim();
    !a.is_empty() && !b.is_empty() && canonical_model_id(a) == canonical_model_id(b)
}

/// Canonical equality honoring the runtime switch. When
/// `case_insensitive` is false this is the legacy exact comparison.
pub fn models_equivalent_with(a: &str, b: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        models_equivalent(a, b)
    } else {
        a.trim() == b.trim() && !a.trim().is_empty()
    }
}

/// Strip a codex subagent variant suffix (`-fast-preview`), then fold the
/// remaining base model to canonical form.
///
/// The suffix is case-sensitive on the wire (it is a gateway-owned marker),
/// so only the exact suffix is stripped before canonical folding.
pub fn canonical_subagent_base_model_id(
    model: &str,
    subagent_suffix: &str,
) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let base_len = model.len().checked_sub(subagent_suffix.len())?;
    let (base, suffix) = model.split_at(base_len);
    if base.is_empty() || suffix != subagent_suffix {
        return None;
    }
    Some(canonical_model_id(base))
}

/// Find the stored spelling (first match) of `model` inside `stored` that is
/// canonical-equivalent to `model` when `case_insensitive` is true, or
/// exactly equal otherwise. Returns the stored spelling unchanged.
pub fn find_equivalent_stored<'a>(
    stored: &'a [String],
    model: &str,
    case_insensitive: bool,
) -> Option<&'a str> {
    if case_insensitive {
        let canonical = canonical_model_id(model);
        if canonical.is_empty() {
            return None;
        }
        stored
            .iter()
            .map(String::as_str)
            .find(|candidate| !candidate.trim().is_empty() && canonical_model_id(candidate) == canonical)
    } else {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        stored.iter().map(String::as_str).find(|candidate| candidate.trim() == model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_model_id_trims_and_lowercases() {
        assert_eq!(canonical_model_id("  GLM-4.5  "), "glm-4.5");
        assert_eq!(canonical_model_id("DeepSeek-V3"), "deepseek-v3");
        assert_eq!(canonical_model_id(""), "");
        assert_eq!(canonical_model_id("   "), "");
    }

    #[test]
    fn models_equivalent_folds_casing_only() {
        assert!(models_equivalent("GLM-4.5", "glm-4.5"));
        assert!(models_equivalent("  DeepSeek-V3 ", "deepseek-v3"));
        assert!(!models_equivalent("glm-4.5", "glm-4-5"));
        assert!(!models_equivalent("glm-4.5", "  "));
        assert!(!models_equivalent("", ""));
    }

    #[test]
    fn models_equivalent_with_switch_degrades_to_exact() {
        assert!(models_equivalent_with("GLM-4.5", "GLM-4.5", false));
        assert!(!models_equivalent_with("GLM-4.5", "glm-4.5", false));
        assert!(models_equivalent_with("GLM-4.5", "glm-4.5", true));
        assert!(!models_equivalent_with("GLM-4.5", "  ", false));
    }

    #[test]
    fn canonical_subagent_base_model_id_strips_suffix_before_folding() {
        let suffix = "-fast-preview";
        assert_eq!(
            canonical_subagent_base_model_id("GLM-4.5-fast-preview", suffix).as_deref(),
            Some("glm-4.5")
        );
        assert_eq!(
            canonical_subagent_base_model_id("glm-4.5", suffix),
            None
        );
        assert_eq!(
            canonical_subagent_base_model_id("-fast-preview", suffix),
            None
        );
        assert_eq!(canonical_subagent_base_model_id("", suffix), None);
    }

    #[test]
    fn find_equivalent_stored_returns_original_spelling() {
        let stored = vec!["GLM-4.5".to_string(), "glm-4.5".to_string(), "other".to_string()];
        assert_eq!(
            find_equivalent_stored(&stored, "glm-4.5", true),
            Some("GLM-4.5")
        );
        assert_eq!(
            find_equivalent_stored(&stored, "GLM-4.5", false),
            Some("GLM-4.5")
        );
        assert_eq!(
            find_equivalent_stored(&stored, "GLM-4.5", true),
            Some("GLM-4.5")
        );
        assert_eq!(find_equivalent_stored(&stored, "missing", true), None);
        assert_eq!(find_equivalent_stored(&stored, "glm-4.5", false), None);
        assert_eq!(find_equivalent_stored(&stored, " ", true), None);
    }
}
