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

/// B2: Model alias rule for explicit model name mapping and display control.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelAliasRule {
    /// The canonical name displayed to downstream and used in all internal keys
    /// (affinity, usage, quotas). This controls the display casing.
    pub canonical: String,
    /// Other spellings that resolve to this canonical name. Matched
    /// case-insensitively against incoming requests and upstream model lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

impl ModelAliasRule {
    /// Validate this rule in isolation (non-empty canonical and aliases).
    pub fn validate_self(&self) -> Result<(), String> {
        let canonical_trimmed = self.canonical.trim();
        if canonical_trimmed.is_empty() {
            return Err("canonical model name cannot be empty or whitespace-only".to_string());
        }
        for (i, alias) in self.aliases.iter().enumerate() {
            if alias.trim().is_empty() {
                return Err(format!("alias at index {} is empty or whitespace-only", i));
            }
        }
        Ok(())
    }
}

/// B2: Model alias registry for resolving explicit alias rules.
#[derive(Clone, Debug, Default)]
pub struct ModelAliasRegistry {
    rules: Vec<ModelAliasRule>,
    /// Precomputed map: canonical lowercase alias → canonical display name.
    /// Built from all rules' `aliases` and `canonical` fields.
    alias_to_canonical: std::collections::HashMap<String, String>,
}

impl ModelAliasRegistry {
    /// Build a registry from a list of rules. Validates that:
    /// - Each alias appears in at most one rule (canonical comparison)
    /// - No canonical name is also an alias in another rule
    /// - No empty strings
    pub fn from_rules(rules: Vec<ModelAliasRule>) -> Result<Self, String> {
        let mut alias_to_canonical: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut seen_canonicals: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for rule in &rules {
            rule.validate_self()?;
            let canonical_key = canonical_model_id(&rule.canonical);
            if canonical_key.is_empty() {
                return Err(format!(
                    "canonical name '{}' becomes empty after trimming",
                    rule.canonical
                ));
            }

            // Check canonical doesn't conflict with an existing alias
            if alias_to_canonical.contains_key(&canonical_key) {
                return Err(format!(
                    "canonical name '{}' is already used as an alias in another rule",
                    rule.canonical
                ));
            }

            seen_canonicals.insert(canonical_key.clone());

            // Register all aliases
            for alias in &rule.aliases {
                let alias_key = canonical_model_id(alias);
                if alias_key.is_empty() {
                    return Err(format!("alias '{}' becomes empty after trimming", alias));
                }

                // Check alias doesn't conflict with a canonical
                if seen_canonicals.contains(&alias_key) {
                    return Err(format!(
                        "alias '{}' conflicts with a canonical name from another rule",
                        alias
                    ));
                }

                if let Some(existing_canonical) = alias_to_canonical.get(&alias_key) {
                    // Within-rule duplicates (same spelling, different casing)
                    // are harmless: they resolve to the same canonical. Only
                    // cross-rule conflicts are rejected.
                    if canonical_model_id(existing_canonical) != canonical_key {
                        return Err(format!(
                            "alias '{}' appears in multiple rules: '{}' and '{}'",
                            alias, existing_canonical, rule.canonical
                        ));
                    }
                    continue;
                }

                alias_to_canonical.insert(alias_key, rule.canonical.clone());
            }
        }

        Ok(Self {
            rules,
            alias_to_canonical,
        })
    }

    /// Resolve a model name to its canonical form according to alias rules.
    /// Returns the rule's `canonical` field if the model matches any alias,
    /// otherwise returns `None` (caller should fall back to B1 case-folding).
    pub fn resolve_alias(&self, model: &str) -> Option<&str> {
        let key = canonical_model_id(model);
        if key.is_empty() {
            return None;
        }
        self.alias_to_canonical.get(&key).map(String::as_str)
    }

    /// Check if a model name matches a canonical name or any of its aliases.
    /// Used for reverse lookup: "does this upstream model spelling map to the
    /// requested canonical?"
    pub fn matches_canonical(&self, model: &str, canonical: &str) -> bool {
        let model_key = canonical_model_id(model);
        let canonical_key = canonical_model_id(canonical);
        if model_key.is_empty() || canonical_key.is_empty() {
            return false;
        }

        // Direct canonical match
        if model_key == canonical_key {
            return true;
        }

        // Check if model is an alias that resolves to this canonical
        if let Some(resolved) = self.alias_to_canonical.get(&model_key) {
            return canonical_model_id(resolved) == canonical_key;
        }

        false
    }

    /// Get all model spellings (canonicals and aliases) that resolve to the
    /// given canonical name, in canonical lowercase form.
    pub fn all_spellings_for_canonical(&self, canonical: &str) -> Vec<String> {
        let canonical_key = canonical_model_id(canonical);
        if canonical_key.is_empty() {
            return Vec::new();
        }

        let mut spellings = vec![canonical_key.clone()];

        for (alias_key, resolved_canonical) in &self.alias_to_canonical {
            if canonical_model_id(resolved_canonical) == canonical_key {
                spellings.push(alias_key.clone());
            }
        }

        spellings
    }

    pub fn rules(&self) -> &[ModelAliasRule] {
        &self.rules
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
        assert_eq!(find_equivalent_stored(&stored, "Glm-4.5", false), None);
        assert_eq!(find_equivalent_stored(&stored, " ", true), None);
    }

    #[test]
    fn model_alias_registry_resolves_aliases() {
        let rules = vec![
            ModelAliasRule {
                canonical: "deepseek-v3".to_string(),
                aliases: vec!["deepseek-chat".to_string(), "DeepSeek-Chat".to_string()],
            },
            ModelAliasRule {
                canonical: "GLM-4.5".to_string(),
                aliases: vec!["glm-4-5".to_string(), "GLM-4.5-Preview".to_string()],
            },
        ];
        let registry = ModelAliasRegistry::from_rules(rules).unwrap();

        assert_eq!(registry.resolve_alias("deepseek-chat"), Some("deepseek-v3"));
        assert_eq!(registry.resolve_alias("DeepSeek-Chat"), Some("deepseek-v3"));
        assert_eq!(registry.resolve_alias("DEEPSEEK-CHAT"), Some("deepseek-v3"));
        assert_eq!(registry.resolve_alias("glm-4-5"), Some("GLM-4.5"));
        assert_eq!(registry.resolve_alias("GLM-4.5-Preview"), Some("GLM-4.5"));
        assert_eq!(registry.resolve_alias("unknown-model"), None);
        assert_eq!(registry.resolve_alias(""), None);
    }

    #[test]
    fn model_alias_registry_matches_canonical() {
        let rules = vec![ModelAliasRule {
            canonical: "deepseek-v3".to_string(),
            aliases: vec!["deepseek-chat".to_string()],
        }];
        let registry = ModelAliasRegistry::from_rules(rules).unwrap();

        assert!(registry.matches_canonical("deepseek-v3", "deepseek-v3"));
        assert!(registry.matches_canonical("DeepSeek-V3", "deepseek-v3"));
        assert!(registry.matches_canonical("deepseek-chat", "deepseek-v3"));
        assert!(registry.matches_canonical("DeepSeek-Chat", "DEEPSEEK-V3"));
        assert!(!registry.matches_canonical("other-model", "deepseek-v3"));
    }

    #[test]
    fn model_alias_registry_rejects_duplicate_aliases() {
        let rules = vec![
            ModelAliasRule {
                canonical: "model-a".to_string(),
                aliases: vec!["shared-alias".to_string()],
            },
            ModelAliasRule {
                canonical: "model-b".to_string(),
                aliases: vec!["shared-alias".to_string()],
            },
        ];
        let err = ModelAliasRegistry::from_rules(rules).unwrap_err();
        assert!(err.contains("appears in multiple rules"));
    }

    #[test]
    fn model_alias_registry_rejects_canonical_as_alias() {
        let rules = vec![
            ModelAliasRule {
                canonical: "model-a".to_string(),
                aliases: vec![],
            },
            ModelAliasRule {
                canonical: "model-b".to_string(),
                aliases: vec!["model-a".to_string()],
            },
        ];
        let err = ModelAliasRegistry::from_rules(rules).unwrap_err();
        assert!(err.contains("conflicts with a canonical"));
    }

    #[test]
    fn model_alias_registry_rejects_empty_strings() {
        let rules = vec![ModelAliasRule {
            canonical: "".to_string(),
            aliases: vec![],
        }];
        let err = ModelAliasRegistry::from_rules(rules).unwrap_err();
        assert!(err.contains("empty"));

        let rules = vec![ModelAliasRule {
            canonical: "valid".to_string(),
            aliases: vec!["".to_string()],
        }];
        let err = ModelAliasRegistry::from_rules(rules).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn model_alias_registry_all_spellings_for_canonical() {
        let rules = vec![ModelAliasRule {
            canonical: "DeepSeek-V3".to_string(),
            aliases: vec!["deepseek-chat".to_string(), "DeepSeek-Chat-V3".to_string()],
        }];
        let registry = ModelAliasRegistry::from_rules(rules).unwrap();

        let mut spellings = registry.all_spellings_for_canonical("deepseek-v3");
        spellings.sort();
        assert_eq!(spellings, vec!["deepseek-chat", "deepseek-chat-v3", "deepseek-v3"]);
    }
}
