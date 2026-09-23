Module OT.
Theorem open_theorem : True. Admitted.
End OT.
Module OL.
Lemma open_lemma : True. Admitted.
End OL.
Module OD.
Definition open_definition : nat. Admitted.
End OD.
Module DT.
Theorem done_theorem : True. Proof. exact I. Qed.
End DT.
Module DL.
Lemma done_lemma : True. Proof. exact I. Qed.
End DL.
Module DD.
Definition done_definition : nat. Proof. exact 42. Defined.
End DD.
Module T1.
Theorem dup_theorem : True. Admitted.
End T1.
Module T2.
Theorem dup_theorem : True. Admitted.
End T2.
Module L1.
Lemma dup_lemma : True. Admitted.
End L1.
Module L2.
Lemma dup_lemma : True. Admitted.
End L2.
Module D1.
Definition dup_definition : nat. Admitted.
End D1.
Module D2.
Definition dup_definition : nat. Admitted.
End D2.
