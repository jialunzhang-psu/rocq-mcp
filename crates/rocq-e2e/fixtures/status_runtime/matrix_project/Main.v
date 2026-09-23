Theorem timeout_true_theorem : True. Admitted.
Lemma timeout_true_lemma : True. Admitted.
Definition timeout_true_definition : True. Admitted.
Theorem timeout_conjunction_theorem : True /\ True. Admitted.
Lemma timeout_conjunction_lemma : True /\ True. Admitted.
Definition timeout_conjunction_definition : True /\ True. Admitted.
Theorem timeout_forall_theorem : forall n : nat, n = n. Admitted.
Lemma timeout_forall_lemma : forall n : nat, n = n. Admitted.
Definition timeout_forall_definition : forall n : nat, n = n. Admitted.
Theorem timeout_definition_theorem : nat. Admitted.
Lemma timeout_definition_lemma : nat. Admitted.
Definition timeout_definition_definition : nat. Admitted.
Theorem done_true : True. Proof. exact I. Qed.
