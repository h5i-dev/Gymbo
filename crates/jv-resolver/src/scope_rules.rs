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
/// `None` stands for upstream's empty scope, which a node has when nothing
/// declared one. It is not the same as `compile`: the default is materialized
/// during model building, and a graph built from a test corpus may legitimately
/// carry nodes with no scope at all.
///
/// # Examples
///
/// ```
/// use jv_model::Scope;
/// use jv_resolver::derive_scope;
///
/// // A compile parent leaves its children alone.
/// assert_eq!(
///     derive_scope(Some(Scope::Compile), Some(Scope::Runtime)),
///     Some(Scope::Runtime)
/// );
/// // A test parent makes everything below it test.
/// assert_eq!(
///     derive_scope(Some(Scope::Test), Some(Scope::Compile)),
///     Some(Scope::Test)
/// );
/// // A provided parent narrows compile and runtime to provided.
/// assert_eq!(
///     derive_scope(Some(Scope::Provided), Some(Scope::Compile)),
///     Some(Scope::Provided)
/// );
/// // A test or system child wins outright, whatever the parent.
/// assert_eq!(
///     derive_scope(Some(Scope::Provided), Some(Scope::Test)),
///     Some(Scope::Test)
/// );
/// ```
pub fn derive_scope(parent: Option<Scope>, child: Option<Scope>) -> Option<Scope> {
    // 1. A test or system child is authoritative; the parent is not consulted.
    if matches!(child, Some(Scope::Test | Scope::System)) {
        return child;
    }
    match parent {
        // 2. The root, and a compile parent, pass the child through unchanged.
        None | Some(Scope::Compile) => child,
        // 3. Test and runtime parents impose themselves.
        Some(scope @ (Scope::Test | Scope::Runtime)) => Some(scope),
        // 4. System and provided parents make everything below them provided.
        Some(Scope::System | Scope::Provided) => Some(Scope::Provided),
        // 5. Any other parent scope yields runtime. Unreachable with the five
        // real scopes: `import` is consumed during model building and never
        // reaches a resolved node.
        Some(Scope::Import) => Some(Scope::Runtime),
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
///     choose_effective_scope(&[Some(Scope::Runtime), Some(Scope::Compile)]),
///     Some(Scope::Compile)
/// );
/// // A lone scope is used as-is.
/// assert_eq!(choose_effective_scope(&[Some(Scope::Test)]), Some(Scope::Test));
/// // System survives only when it is the only candidate.
/// assert_eq!(
///     choose_effective_scope(&[Some(Scope::System)]),
///     Some(Scope::System)
/// );
/// assert_eq!(
///     choose_effective_scope(&[Some(Scope::System), Some(Scope::Test)]),
///     Some(Scope::Test)
/// );
/// ```
pub fn choose_effective_scope(scopes: &[Option<Scope>]) -> Option<Scope> {
    let mut unique: Vec<Option<Scope>> = Vec::new();
    for scope in scopes {
        if !unique.contains(scope) {
            unique.push(*scope);
        }
    }
    // System yields to anything else present.
    if unique.len() > 1 {
        unique.retain(|scope| *scope != Some(Scope::System));
    }
    if unique.len() == 1 {
        return unique[0];
    }
    // Widest wins, in this order.
    [Scope::Compile, Scope::Runtime, Scope::Provided, Scope::Test]
        .into_iter()
        .find(|candidate| unique.contains(&Some(*candidate)))
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
                    derive_scope(parent, Some(*child)),
                    Some(expected[index]),
                    "parent {parent:?} with child {child:?}"
                );
            }
        }
    }

    #[test]
    fn a_child_with_no_scope_follows_the_parent() {
        // An unscoped node is upstream's empty scope, not compile: a compile or
        // absent parent leaves it empty, and a narrowing parent still narrows it.
        assert_eq!(derive_scope(None, None), None);
        assert_eq!(derive_scope(Some(Scope::Compile), None), None);
        assert_eq!(derive_scope(Some(Scope::Test), None), Some(Scope::Test));
        assert_eq!(
            derive_scope(Some(Scope::Provided), None),
            Some(Scope::Provided)
        );
    }

    #[test]
    fn a_test_parent_makes_its_whole_subtree_test() {
        // The rule people rely on without noticing: nothing under a test
        // dependency reaches the compile path.
        for child in REAL {
            let derived = derive_scope(Some(Scope::Test), Some(child));
            assert!(
                derived == Some(Scope::Test) || derived == Some(Scope::System),
                "{child:?} under a test parent derived to {derived:?}"
            );
        }
    }

    #[test]
    fn derivation_composes_down_a_path() {
        // root -> provided -> compile -> compile
        let first = derive_scope(None, Some(Scope::Provided));
        let second = derive_scope(first, Some(Scope::Compile));
        let third = derive_scope(second, Some(Scope::Compile));
        assert_eq!(first, Some(Scope::Provided));
        assert_eq!(second, Some(Scope::Provided));
        assert_eq!(third, Some(Scope::Provided));
    }

    #[test]
    fn effective_scope_prefers_the_widest() {
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Test), Some(Scope::Compile)]),
            Some(Scope::Compile)
        );
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Test), Some(Scope::Runtime)]),
            Some(Scope::Runtime)
        );
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Test), Some(Scope::Provided)]),
            Some(Scope::Provided)
        );
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Test)]),
            Some(Scope::Test)
        );
    }

    #[test]
    fn system_yields_to_anything_else() {
        assert_eq!(
            choose_effective_scope(&[Some(Scope::System), Some(Scope::Compile)]),
            Some(Scope::Compile)
        );
        assert_eq!(
            choose_effective_scope(&[Some(Scope::System), Some(Scope::Test)]),
            Some(Scope::Test)
        );
        // Alone, it stands.
        assert_eq!(
            choose_effective_scope(&[Some(Scope::System)]),
            Some(Scope::System)
        );
    }

    #[test]
    fn duplicates_do_not_change_the_answer() {
        assert_eq!(
            choose_effective_scope(&[
                Some(Scope::Runtime),
                Some(Scope::Runtime),
                Some(Scope::Runtime)
            ]),
            Some(Scope::Runtime)
        );
    }

    #[test]
    fn nothing_to_choose_from() {
        assert_eq!(choose_effective_scope(&[]), None);
        // A lone empty scope stays empty.
        assert_eq!(choose_effective_scope(&[None]), None);
    }

    #[test]
    fn an_empty_scope_loses_to_a_ranked_one() {
        assert_eq!(
            choose_effective_scope(&[None, Some(Scope::Test)]),
            Some(Scope::Test)
        );
    }

    #[test]
    fn a_lone_unusual_scope_is_used_verbatim() {
        // Upstream returns a single remaining scope even when it is not one of
        // the four it ranks.
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Import)]),
            Some(Scope::Import)
        );
        // But it loses to a ranked one when both are present.
        assert_eq!(
            choose_effective_scope(&[Some(Scope::Import), Some(Scope::Test)]),
            Some(Scope::Test)
        );
    }
}
