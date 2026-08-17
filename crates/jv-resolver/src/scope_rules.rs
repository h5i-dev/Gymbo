//! How scopes travel down the graph, and which one a winner ends up with.
//!
//! Port of `JavaScopeDeriver` and `JavaScopeSelector`, using Maven 3.9's table.
//! These two rules decide the `:compile` / `:test` suffix on every line of
//! `mvn dependency:tree`, so they are worth stating precisely.
//!
//! The derivation reads in one direction only: a parent's scope can *narrow* a
//! child's, never widen it. A `test` dependency's own dependencies are `test`
//! whatever they declared, which is why a library pulled in through a test
//! dependency does not end up on the compile path.

use jv_model::Scope;

/// The scope a child ends up with, given the scope its parent was derived to.
///
/// `parent` is the parent's *already derived* scope, not what the parent
/// declared — derivation composes down a path.
///
/// # Examples
///
/// ```
/// use jv_model::Scope;
/// use jv_resolver::derive_scope;
///
/// // A compile parent leaves its children alone.
/// assert_eq!(derive_scope(Some(Scope::Compile), Scope::Runtime), Scope::Runtime);
/// // A test parent makes everything below it test.
/// assert_eq!(derive_scope(Some(Scope::Test), Scope::Compile), Scope::Test);
/// // A provided parent narrows compile and runtime to provided.
/// assert_eq!(derive_scope(Some(Scope::Provided), Scope::Compile), Scope::Provided);
/// // A test or system child wins outright, whatever the parent.
/// assert_eq!(derive_scope(Some(Scope::Provided), Scope::Test), Scope::Test);
/// ```
pub fn derive_scope(parent: Option<Scope>, child: Scope) -> Scope {
    // 1. A test or system child is authoritative; the parent is not consulted.
    if matches!(child, Scope::Test | Scope::System) {
        return child;
    }
    match parent {
        // 2. The root, and a compile parent, pass the child through unchanged.
        None | Some(Scope::Compile) => child,
        // 3. Test and runtime parents impose themselves.
        Some(scope @ (Scope::Test | Scope::Runtime)) => scope,
        // 4. System and provided parents make everything below them provided.
        Some(Scope::System | Scope::Provided) => Scope::Provided,
        // 5. Any other parent scope yields runtime. Unreachable with the five
        // real scopes: `import` is consumed during model building and never
        // reaches a resolved node.
        Some(Scope::Import) => Scope::Runtime,
    }
}

/// The scope a winning node takes, given every derived scope its occurrences
/// had.
///
/// Returns `None` when there is nothing to choose from, which upstream spells as
/// the empty scope.
///
/// The order is *widest wins*: compile, then runtime, then provided, then test.
/// `system` is dropped whenever anything else is present, because a system
/// dependency reached by another route is resolvable normally.
///
/// This is only half the rule. The other half lives in the resolver: an
/// occurrence at depth 0 or 1 short-circuits everything, because a direct
/// dependency's declared scope is authoritative and is never widened by a
/// transitive path.
///
/// # Examples
///
/// ```
/// use jv_model::Scope;
/// use jv_resolver::choose_effective_scope;
///
/// // Reached at compile through one path and runtime through another.
/// assert_eq!(
///     choose_effective_scope(&[Scope::Runtime, Scope::Compile]),
///     Some(Scope::Compile)
/// );
/// // A lone scope is used as-is.
/// assert_eq!(choose_effective_scope(&[Scope::Test]), Some(Scope::Test));
/// // System survives only when it is the only candidate.
/// assert_eq!(choose_effective_scope(&[Scope::System]), Some(Scope::System));
/// assert_eq!(
///     choose_effective_scope(&[Scope::System, Scope::Test]),
///     Some(Scope::Test)
/// );
/// ```
pub fn choose_effective_scope(scopes: &[Scope]) -> Option<Scope> {
    let mut unique: Vec<Scope> = Vec::new();
    for scope in scopes {
        if !unique.contains(scope) {
            unique.push(*scope);
        }
    }
    // System yields to anything else present.
    if unique.len() > 1 {
        unique.retain(|scope| *scope != Scope::System);
    }
    if unique.len() == 1 {
        return unique.first().copied();
    }
    // Widest wins, in this order.
    [Scope::Compile, Scope::Runtime, Scope::Provided, Scope::Test]
        .into_iter()
        .find(|candidate| unique.contains(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five scopes that can appear on a resolved node.
    const REAL: [Scope; 5] = [
        Scope::Compile,
        Scope::Provided,
        Scope::Runtime,
        Scope::Test,
        Scope::System,
    ];

    /// The whole matrix from the specification, as a table so a wrong cell is
    /// visible rather than merely failing.
    ///
    /// Rows are the parent's derived scope, columns are the child's declared
    /// scope, in the order of [`REAL`].
    #[test]
    fn derivation_matches_the_maven_3_matrix() {
        use Scope::{Compile, Provided, Runtime, System, Test};
        let rows: [(Option<Scope>, [Scope; 5]); 6] = [
            //  parent            compile   provided  runtime   test  system
            (None, [Compile, Provided, Runtime, Test, System]),
            (Some(Compile), [Compile, Provided, Runtime, Test, System]),
            (Some(Provided), [Provided, Provided, Provided, Test, System]),
            (Some(Runtime), [Runtime, Runtime, Runtime, Test, System]),
            (Some(Test), [Test, Test, Test, Test, System]),
            (Some(System), [Provided, Provided, Provided, Test, System]),
        ];

        for (parent, expected) in rows {
            for (index, child) in REAL.iter().enumerate() {
                assert_eq!(
                    derive_scope(parent, *child),
                    expected[index],
                    "parent {parent:?} with child {child:?}"
                );
            }
        }
    }

    #[test]
    fn a_test_parent_makes_its_whole_subtree_test() {
        // The rule people rely on without noticing: nothing under a test
        // dependency reaches the compile path.
        for child in REAL {
            let derived = derive_scope(Some(Scope::Test), child);
            assert!(
                derived == Scope::Test || derived == Scope::System,
                "{child:?} under a test parent derived to {derived:?}"
            );
        }
    }

    #[test]
    fn derivation_composes_down_a_path() {
        // root -> provided -> compile -> compile
        let first = derive_scope(None, Scope::Provided);
        let second = derive_scope(Some(first), Scope::Compile);
        let third = derive_scope(Some(second), Scope::Compile);
        assert_eq!(first, Scope::Provided);
        assert_eq!(second, Scope::Provided);
        assert_eq!(third, Scope::Provided);
    }

    #[test]
    fn effective_scope_prefers_the_widest() {
        assert_eq!(
            choose_effective_scope(&[Scope::Test, Scope::Compile]),
            Some(Scope::Compile)
        );
        assert_eq!(
            choose_effective_scope(&[Scope::Test, Scope::Runtime]),
            Some(Scope::Runtime)
        );
        assert_eq!(
            choose_effective_scope(&[Scope::Test, Scope::Provided]),
            Some(Scope::Provided)
        );
        assert_eq!(choose_effective_scope(&[Scope::Test]), Some(Scope::Test));
    }

    #[test]
    fn system_yields_to_anything_else() {
        assert_eq!(
            choose_effective_scope(&[Scope::System, Scope::Compile]),
            Some(Scope::Compile)
        );
        assert_eq!(
            choose_effective_scope(&[Scope::System, Scope::Test]),
            Some(Scope::Test)
        );
        // Alone, it stands.
        assert_eq!(
            choose_effective_scope(&[Scope::System]),
            Some(Scope::System)
        );
    }

    #[test]
    fn duplicates_do_not_change_the_answer() {
        assert_eq!(
            choose_effective_scope(&[Scope::Runtime, Scope::Runtime, Scope::Runtime]),
            Some(Scope::Runtime)
        );
    }

    #[test]
    fn nothing_to_choose_from() {
        assert_eq!(choose_effective_scope(&[]), None);
    }

    #[test]
    fn a_lone_unusual_scope_is_used_verbatim() {
        // Upstream returns a single remaining scope even when it is not one of
        // the four it ranks.
        assert_eq!(
            choose_effective_scope(&[Scope::Import]),
            Some(Scope::Import)
        );
        // But it loses to a ranked one when both are present.
        assert_eq!(
            choose_effective_scope(&[Scope::Import, Scope::Test]),
            Some(Scope::Test)
        );
    }
}
