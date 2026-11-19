----------------------------- MODULE Private -------------------------------
EXTENDS Naturals

\* One session, one curated case, one frozen experiment. A successful
\* curated preview represents the backend's already minimized projection.
\* An edit/freeze races a change between preview and commit. Physical source
\* loss blocks revalidation, while confirmed local-history purge cascades.
VARIABLES owner, available, anchor, rating, dataset, intent, acknowledged, curated, copy, expected
vars == <<owner,available,anchor,rating,dataset,intent,acknowledged,curated,copy,expected>>
Intent(k) == [kind |-> k, owner |-> owner, anchor |-> anchor,
              rating |-> rating, dataset |-> dataset]
Idle == [kind |-> "none",owner |-> 0,anchor |-> 0,rating |-> 0,dataset |-> 0]
Init ==
  /\ owner = 1 /\ available = TRUE /\ anchor = 1 /\ rating = 1 /\ dataset = 0
  /\ intent = Idle /\ acknowledged = FALSE /\ curated = 0 /\ copy = 0 /\ expected = 0
Prepare(k) ==
  /\ owner # 0 /\ intent = Idle
  /\ CASE k = "curate" -> available /\ curated = 0 /\ dataset < 3
       [] k = "freeze" -> curated # 0 /\ expected = 0
       [] OTHER -> rating = 1
  /\ intent' = Intent(k) /\ acknowledged' = FALSE
  /\ UNCHANGED <<owner,available,anchor,rating,dataset,curated,copy,expected>>
Review ==
  /\ intent.kind \in {"curate","freeze"} /\ ~acknowledged /\ acknowledged' = TRUE
  /\ UNCHANGED <<owner,available,anchor,rating,dataset,intent,curated,copy,expected>>
Commit ==
  /\ intent # Idle /\ intent' = Idle /\ acknowledged' = FALSE
  /\ (intent.kind = "edit" \/ acknowledged)
  /\ LET sameOwner == owner # 0 /\ owner = intent.owner
         edit == intent.kind = "edit" /\ sameOwner /\ rating = intent.rating
         create == intent.kind = "curate" /\ sameOwner /\ acknowledged /\ available
                   /\ anchor = intent.anchor /\ rating = intent.rating /\ dataset = intent.dataset
         freeze == intent.kind = "freeze" /\ acknowledged /\ curated # 0 /\ dataset = intent.dataset
     IN /\ rating' = IF edit THEN rating+1 ELSE rating
        /\ curated' = IF create THEN owner ELSE curated
        /\ dataset' = IF create THEN dataset+1 ELSE dataset
        /\ copy' = IF freeze THEN curated ELSE copy
        /\ expected' = IF freeze THEN 1 ELSE expected
  /\ UNCHANGED <<owner,available,anchor>>
OtherRating ==
  /\ owner # 0 /\ rating = 1 /\ rating' = 2
  /\ UNCHANGED <<owner,available,anchor,dataset,intent,acknowledged,curated,copy,expected>>
OtherDataset ==
  /\ dataset = 0 /\ dataset' = 1
  /\ UNCHANGED <<owner,available,anchor,rating,intent,acknowledged,curated,copy,expected>>
Rewrite ==
  /\ anchor = 1 /\ available /\ anchor' = 2
  /\ UNCHANGED <<owner,available,rating,dataset,intent,acknowledged,curated,copy,expected>>
SourceLost ==
  /\ available /\ available' = FALSE
  /\ UNCHANGED <<owner,anchor,rating,dataset,intent,acknowledged,curated,copy,expected>>
DeleteCase ==
  /\ curated # 0 /\ curated' = 0 /\ copy' = 0 /\ dataset' = dataset+1
  /\ UNCHANGED <<owner,available,anchor,rating,intent,acknowledged,expected>>
Purge ==
  /\ owner = 1 /\ owner' = 0 /\ available' = FALSE /\ curated' = 0 /\ copy' = 0
  /\ dataset' = IF curated # 0 THEN dataset+1 ELSE dataset
  /\ UNCHANGED <<anchor,rating,intent,acknowledged,expected>>
Reuse ==
  /\ owner = 0 /\ owner' = 2 /\ available' = TRUE /\ anchor' = 1 /\ rating' = 1
  /\ UNCHANGED <<dataset,intent,acknowledged,curated,copy,expected>>
Next == (\E k \in {"edit","curate","freeze"}: Prepare(k))
        \/ Review \/ Commit \/ OtherRating \/ OtherDataset
        \/ Rewrite \/ SourceLost \/ DeleteCase \/ Purge \/ Reuse \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
Cascades ==
  /\ (curated # 0 => curated = owner)
  /\ (copy # 0 => expected = 1 /\ copy = curated)
KeepDenominator == [][expected' >= expected]_vars
CreateUsesReview ==
  [] [(curated = 0 /\ curated' # 0 =>
       /\ intent.kind = "curate" /\ acknowledged /\ available /\ owner = intent.owner
       /\ anchor = intent.anchor /\ rating = intent.rating /\ dataset = intent.dataset)]_vars
FreezeUsesReview ==
  [] [(expected = 0 /\ expected' = 1 =>
       acknowledged /\ intent.kind = "freeze" /\ dataset = intent.dataset /\ curated # 0)]_vars
EditorFresh ==
  [] [(intent.kind = "edit" /\ intent' = Idle /\ rating' > rating =>
       owner = intent.owner /\ rating = intent.rating)]_vars
=============================================================================
