//! Guards that keep const-local lambdas lazy while failing their reads of
//! non-const members when invoked.

use super::*;

/// Keep const-local lambdas lazy while making a known non-const member read
/// fail at invocation. The member set contains only values that actually
/// bound, so unused lambdas and untaken generator branches remain valid.
impl Evaluator {
    /// Remove guards for lambdas which have no remaining value holding their
    /// capture map. Keeping live guards is necessary because callers may keep
    /// a lambda returned by an earlier evaluation and invoke it after reusing
    /// this evaluator.
    pub(super) fn prune_lambda_guards(&mut self) {
        #[cfg(test)]
        {
            self.lambda_guard_prune_scans += self.lambda_guards.len();
        }
        self.lambda_guards
            .retain(|_, entry| entry.captured.upgrade().is_some());
        self.lambda_guard_prune_at = self
            .lambda_guards
            .len()
            .saturating_mul(2)
            .max(LAMBDA_GUARD_PRUNE_MIN);
    }

    pub(super) fn register_lambda_guard(&mut self, captured: &Arc<ScopeMap>) {
        // A single evaluation can create and immediately discard many
        // closures. Clean periodically rather than before every registration:
        // dead entries stay bounded by the registry's recent high-water mark,
        // and evaluation boundaries remove them all. Long-lived closure
        // construction therefore stays amortized linear.
        if self.lambda_guards.len() >= self.lambda_guard_prune_at {
            self.prune_lambda_guards();
        }
        self.lambda_guards.insert(
            Arc::as_ptr(captured) as usize,
            LambdaGuardEntry {
                captured: Arc::downgrade(captured),
                guard: LambdaGuard::default(),
            },
        );
    }

    pub(super) fn lambda_guard(&self, captured: &Arc<ScopeMap>) -> Option<LambdaGuard> {
        let entry = self.lambda_guards.get(&(Arc::as_ptr(captured) as usize))?;
        entry
            .captured
            .upgrade()
            .filter(|registered| Arc::ptr_eq(registered, captured))
            .map(|_| entry.guard.clone())
    }

    pub(super) fn lambda_guard_error(&self, captured: &Arc<ScopeMap>) -> Option<Arc<str>> {
        self.lambda_guard(captured).and_then(|guard| guard.error())
    }

    pub(super) fn const_local_lambda_guard(&self, value: Value, message: Option<String>) -> Value {
        let Some(message) = message else {
            return value;
        };
        if let Value::Lambda(_, _, captured) = &value
            && let Some(guard) = self.lambda_guard(captured)
        {
            guard.set_error(message);
        }
        value
    }

    pub(super) fn guard_const_local_lambdas(
        &self,
        scope: &mut Scope,
        deferred_lambdas: &[(Name, &Property, &Expr, usize)],
        non_const_members: &HashSet<Name>,
    ) {
        for (name, prop, _, _) in deferred_lambdas {
            let Some(message) = const_local_reads_non_const(prop, non_const_members) else {
                continue;
            };
            let Some(value) = scope.get(name).cloned() else {
                continue;
            };
            scope.set(
                name.clone(),
                self.const_local_lambda_guard(value, Some(message)),
            );
        }
    }
}

/// The smallest number of weak lambda-guard entries retained between cleanup
/// passes. Larger live sets use a doubling threshold for amortized cleanup;
/// an evaluation boundary always removes dropped entries.
pub(super) const LAMBDA_GUARD_PRUNE_MIN: usize = 64;

/// A delayed evaluation error shared by every copy of one lambda value.
#[derive(Debug, Clone, Default)]
pub(super) struct LambdaGuard(Arc<RwLock<Option<Arc<str>>>>);

impl LambdaGuard {
    fn error(&self) -> Option<Arc<str>> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_error(&self, message: String) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message.into());
    }
}

/// Guard state stays in the evaluator rather than a lambda's captured scope,
/// so it cannot become observable through `outer`.
pub(super) struct LambdaGuardEntry {
    captured: Weak<ScopeMap>,
    guard: LambdaGuard,
}

#[cfg(all(test, feature = "native-io"))]
mod lambda_guard_tests {
    use std::{path::Path, sync::Arc};

    use super::{Evaluator, LAMBDA_GUARD_PRUNE_MIN};
    use crate::{Value, value::ScopeMap};

    #[test]
    fn lambda_guards_prune_dead_captures_but_keep_live_diagnostics() {
        let mut evaluator = Evaluator::default();

        // Registering transient closures during one evaluation must not keep a
        // weak registry entry for each closure that has already been dropped.
        for _ in 0..LAMBDA_GUARD_PRUNE_MIN {
            let captured = Arc::new(ScopeMap::default());
            evaluator.register_lambda_guard(&captured);
        }
        assert!(
            evaluator.lambda_guards.len() <= LAMBDA_GUARD_PRUNE_MIN,
            "dead lambda guard entries accumulated: {}",
            evaluator.lambda_guards.len()
        );

        // A returned lambda remains live across evaluator reuse, so its
        // delayed const-local diagnostic must survive the next evaluation.
        let value = evaluator
            .eval_source(
                r#"
foo {
  const local f = () -> res1
  res1 = 15
  result = f
}
"#,
                Path::new("lambda-guard.pkl"),
            )
            .unwrap();
        let Value::Object(module, _) = &value else {
            panic!("expected module object");
        };
        let Value::Object(foo, _) = &module["foo"] else {
            panic!("expected foo object");
        };
        let Value::Lambda(_, _, captured) = &foo["result"] else {
            panic!("expected result lambda");
        };
        let lambda = foo["result"].clone();
        let expected = "Cannot reference property `res1` from here because it is not `const`.";
        assert_eq!(
            evaluator.lambda_guard_error(captured).as_deref(),
            Some(expected)
        );
        drop(value);

        evaluator
            .eval_source("next = 1\n", Path::new("next-evaluation.pkl"))
            .unwrap();
        let error = evaluator
            .invoke_lambda(&lambda, &[], 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "unexpected lambda error: {error}");

        drop(lambda);
        evaluator.begin_evaluation();
        assert!(evaluator.lambda_guards.is_empty());
    }

    #[test]
    fn lambda_guard_cleanup_is_amortized_for_retained_closures() {
        let mut evaluator = Evaluator::default();
        let mut retained = Vec::new();
        let count = LAMBDA_GUARD_PRUNE_MIN * 16 + 1;

        for _ in 0..count {
            let captured = Arc::new(ScopeMap::default());
            evaluator.register_lambda_guard(&captured);
            retained.push(captured);
        }

        assert_eq!(evaluator.lambda_guards.len(), retained.len());
        assert!(
            evaluator.lambda_guard_prune_scans <= count * 2,
            "cleanup scanned {} entries while retaining {count} closures",
            evaluator.lambda_guard_prune_scans
        );
    }

    #[test]
    fn lambda_guard_cleanup_reclaims_dropped_high_water_mark_at_next_evaluation() {
        let mut evaluator = Evaluator::default();
        let mut captures = Vec::new();

        for _ in 0..LAMBDA_GUARD_PRUNE_MIN * 2 {
            let captured = Arc::new(ScopeMap::default());
            evaluator.register_lambda_guard(&captured);
            captures.push(captured);
        }
        let survivor = captures.pop().unwrap();
        evaluator
            .lambda_guard(&survivor)
            .unwrap()
            .set_error("survives cleanup".into());
        drop(captures);

        evaluator.begin_evaluation();
        assert_eq!(evaluator.lambda_guards.len(), 1);
        assert_eq!(
            evaluator.lambda_guard_error(&survivor).as_deref(),
            Some("survives cleanup")
        );
    }
}
