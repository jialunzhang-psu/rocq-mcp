# Induction Generalization Patterns

## Choose the Induction Target

- Induct on the data that structurally changes in recursive definitions.
- Induct on the derivation when the proof follows constructors of a relation.
- Induct on the operational step when each runtime rule determines the case.
- For mutually inductive relations, use the combined induction principle.

## Revert Before Induction

If `y` depends on the induction target, or the IH must apply to arbitrary `y`:

```coq
revert y.
induction x; intros y.
```

If `y` appears in hypotheses:

```coq
generalize dependent y.
induction x; intros y Hy.
```

## Strengthen the Statement

If the IH cannot be applied because the conclusion is too specific, stop adding
tactics and prove a generalized helper. Common strengthening moves:

- quantify over more variables,
- make an accumulator arbitrary,
- state preservation for all post-contexts satisfying a relation,
- prove a mutual lemma for related judgments together.

## Strong or Well-Founded Induction

Use when recursive calls are on smaller values not syntactic subterms:

```coq
induction n as [n IH] using lt_wf_ind.
```

Keep the proof obligation explicit; avoid hiding the measure in automation.
