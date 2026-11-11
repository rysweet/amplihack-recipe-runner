//! Typed replay helpers used by the existing execution loops.
use super::{ipc::Client, model::*, resources};
use crate::{
    context::RecipeContext,
    models::{Recipe, Step, StepResult, StepType},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[allow(clippy::too_many_arguments)]
pub fn probe(
    recipe: &Recipe,
    step: &Step,
    index: usize,
    path: &str,
    ctx: &RecipeContext,
    working_dir: &str,
    settings: &str,
    coverage: Value,
) -> Result<Probe> {
    let policy = recipe.recovery.as_ref().filter(|p| p.version == 1);
    let text = serde_json::to_string(&(step, &recipe.hooks))?;
    let mut inputs = BTreeMap::new();
    let mut permitted = policy.is_some()
        && step
            .recovery
            .as_ref()
            .is_some_and(|p| p.replay == Replay::Reuse)
        && policy.is_some_and(|p| p.persist.outputs.contains(&step.id));
    // Shell helpers can inspect the entire environment, use indirect expansion,
    // or read a context transport. Syntax alone cannot prove non-consumption.
    let opaque_shell = step.effective_type() == StepType::Bash
        || recipe.hooks.pre_step.is_some()
        || recipe.hooks.post_step.is_some();
    for (key, value) in ctx.data() {
        let used = opaque_shell
            || text.contains(&format!("{{{{{key}"))
            || text.contains(&format!("${}", key.to_uppercase()))
            || text.contains(&format!("${{{}", key.to_uppercase()))
            || text.contains(&format!("$RECIPE_VAR_{key}"))
            || text.contains(&format!("${{RECIPE_VAR_{key}"))
            || step.condition.as_ref().is_some_and(|c| c.contains(key))
            || step.effective_type() == StepType::Recipe;
        if used {
            if policy.is_some_and(|p| p.persist.context.contains(key)) {
                inputs.insert(key.clone(), value.clone());
            } else {
                permitted = false;
            }
        }
    }
    let resources = resources::observe(
        step.recovery
            .as_ref()
            .map_or(&[][..], |p| p.resources.as_slice()),
        ctx,
        &ctx.render(step.working_dir.as_deref().unwrap_or(working_dir)),
    );
    if resources.iter().any(|r| r.kind == "live") {
        permitted = false;
    }
    let header = json!({"name":recipe.name,"version":recipe.version,"hooks":recipe.hooks,"recursion":recipe.recursion,"validation":recipe.context_validation,"policy":recipe.recovery});
    Ok(Probe {
        rebind: step
            .recovery
            .as_ref()
            .is_some_and(|p| p.replay == Replay::Rebind),
        definitions: BTreeMap::new(),
        location: Location {
            invocation_path: path.into(),
            step_id: step.id.clone(),
        },
        index,
        group: step.parallel_group.clone(),
        // Version the coverage rule: older records cannot prove complete
        // shell dependencies, even when their recorded values happen to match.
        definition: fingerprint(&("semantic-input-coverage-v2", header, step, coverage))?,
        inputs,
        permitted,
        resources,
        settings: {
            let observed: BTreeMap<_, _> = std::env::vars_os()
                .filter_map(|(name, value)| {
                    let name = name.into_string().ok()?;
                    let referenced =
                        text.contains(&format!("${name}")) || text.contains(&format!("${{{name}"));
                    // Context aliases are supplied by the current RecipeContext,
                    // rather than inherited process values. Compare those above.
                    let context_alias = ctx.data().keys().any(|key| {
                        name == key.to_uppercase() || name == format!("RECIPE_VAR_{key}")
                    });
                    (referenced && !context_alias).then(|| (name, digest(value.as_encoded_bytes())))
                })
                .collect();
            fingerprint(&(settings, observed))?
        },
    })
}
pub fn apply(ctx: &mut RecipeContext, record: &Record) {
    let mut data = ctx.to_map();
    for (k, v) in &record.delta {
        match v {
            ContextChange::Assign { value } => {
                data.insert(k.clone(), value.clone());
            }
            ContextChange::Remove => {
                data.remove(k);
            }
        }
    }
    *ctx = RecipeContext::new(data);
}
pub fn finalize(
    client: &Client,
    definition: (&Recipe, &Step),
    mut probe: Probe,
    before: &RecipeContext,
    after: &RecipeContext,
    result: &StepResult,
    working_dir: &str,
) -> Result<()> {
    let (recipe, step) = definition;
    if !matches!(
        result.status,
        crate::models::StepStatus::Completed | crate::models::StepStatus::Skipped
    ) {
        probe.permitted = false;
    }
    let allowed = recipe.recovery.as_ref().map(|p| &p.persist.context);
    let mut delta = BTreeMap::new();
    for (key, value) in after.data() {
        if before.data().get(key) != Some(value) {
            if !allowed.is_some_and(|a| a.contains(key)) {
                probe.permitted = false;
            } else {
                delta.insert(
                    key.clone(),
                    ContextChange::Assign {
                        value: value.clone(),
                    },
                );
            }
        }
    }
    for key in before.data().keys() {
        if !after.data().contains_key(key) {
            if !allowed.is_some_and(|a| a.contains(key)) {
                probe.permitted = false;
            } else {
                delta.insert(key.clone(), ContextChange::Remove);
            }
        }
    }
    // Resolve output-bound artifacts after assignment, before publication.
    probe.resources = resources::observe(
        step.recovery
            .as_ref()
            .map_or(&[][..], |p| p.resources.as_slice()),
        after,
        &after.render(step.working_dir.as_deref().unwrap_or(working_dir)),
    );
    if probe.resources.iter().any(|r| {
        r.context_key
            .as_ref()
            .is_some_and(|key| !allowed.is_some_and(|a| a.contains(key)))
    }) {
        probe.permitted = false;
    }
    client.commit(Record {
        child_resources: vec![],
        child_definitions: BTreeMap::new(),
        probe,
        result: result.clone(),
        delta,
        descendants: vec![],
    })
}
/// Root admission handles outcome recognition. Nested runners preserve causes in
/// their normal structured errors, so a stale child blockage cannot win later.
pub fn usage_limit(result: &crate::models::RecipeResult) -> bool {
    !result.success
        && result
            .step_results
            .iter()
            .filter(|s| s.status == crate::models::StepStatus::Failed)
            .any(|s| {
                let text = s.error.to_lowercase();
                text.contains("you've hit your session limit")
                    || text.contains("you've hit your limit")
                    || text.contains("you've reached your usage limit")
            })
}
