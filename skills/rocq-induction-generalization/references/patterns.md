# Induction and Generalization Facts

- The induction target for a recursive data definition is the data argument
  whose constructors determine recursive subcases.
- The induction target for an inductive relation is a derivation of that
  relation.
- A parameter that appears in local hypotheses can be generalized with
  `generalize dependent` before induction.
- A generalized helper can quantify over additional variables, an accumulator,
  a post-context, or mutually related judgments.
- Well-founded induction uses a proof that each recursive case is smaller in a
  well-founded relation.

```coq
induction n as [n IH] using lt_wf_ind.
```
