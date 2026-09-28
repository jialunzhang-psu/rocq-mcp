# Recursion Termination Patterns

## Structural Recursion

Make recursive calls on direct subterms:

```coq
Fixpoint f (xs : list A) : B :=
  match xs with
  | [] => ...
  | x :: xs' => f xs'
  end.
```

If needed:

```coq
Fixpoint f (x : T) {struct x} : U := ...
```

## Fuel

Use fuel when the recursive structure is operational rather than structural:

```coq
Fixpoint check_fuel (fuel : nat) (...) :=
  match fuel with
  | 0 => None
  | S fuel' => ...
  end.
```

Then prove a separate soundness lemma for the fuelled checker.

## Well-Founded Recursion

Use when a measure decreases:

```coq
induction n as [n IH] using lt_wf_ind.
```

For definitions, consider `Program Fixpoint` only if the generated obligations
are simpler than a relation plus soundness proof.

## Avoid

Do not disable guard checking. Do not hide nontermination behind axioms.
