Theorem open_true : True. Admitted.
Theorem done_true : True. Proof. exact I. Qed.
Definition answer : nat. Proof. exact 42. Defined.
Axiom allowed_axiom : True.
Theorem duplicate : True. Admitted.
Module Nested.
Theorem duplicate : True. Admitted.
End Nested.
