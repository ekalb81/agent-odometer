------------------------------ MODULE Parser ------------------------------
EXTENDS Naturals, Sequences

\* Synthetic record classes. A physical line may arrive in two writes. The
\* parser cursor counts complete records instead of bytes. Full rescans reset
\* the cursor, streamed-message ID set and projection together.
Providers == {"codex", "claude", "gemini"}
Records(p) ==
  CASE p = "codex" -> <<"meta", "human", "codex3", "meta", "bad", "ignored",
                         "switch", "codex5", "abort", "rollback", "human", "complete">>
    [] p = "claude" -> <<"meta", "human", "msg1", "toolResult", "msg1", "bad",
                          "synthetic", "switch", "msg2", "ignored">>
    [] OTHER -> <<"meta", "human", "msg1", "msg1", "bad", "ignored",
                  "switch", "msg2">>

VARIABLES provider, sealed, partial, cursor, total, buckets, seen, model, turn
vars == <<provider, sealed, partial, cursor, total, buckets, seen, model, turn>>
ResetProjection ==
  /\ cursor = 0 /\ total = 0 /\ buckets = <<0, 0>> /\ seen = {}
  /\ model = 1 /\ turn = "none"
Init ==
  /\ provider \in Providers /\ sealed = 0 /\ partial = FALSE /\ ResetProjection
BeginLine ==
  /\ sealed < Len(Records(provider)) /\ ~partial /\ partial' = TRUE
  /\ UNCHANGED <<provider, sealed, cursor, total, buckets, seen, model, turn>>
Newline ==
  /\ partial /\ sealed' = sealed + 1 /\ partial' = FALSE
  /\ UNCHANGED <<provider, cursor, total, buckets, seen, model, turn>>
Parse ==
  /\ cursor < sealed
  /\ LET record == Records(provider)[cursor+1]
         message == record \in {"msg1", "msg2"}
         delta == CASE record = "msg1" -> 1 [] record = "msg2" -> 2 [] OTHER -> 0
         \* Received cumulative and last-call counts can differ on resume.
         cumulative == IF record = "codex3" THEN 3 ELSE 5
         lastCall == IF record = "codex3" THEN 1 ELSE 2
         contribution ==
           IF record \in {"codex3", "codex5"}
           THEN lastCall + (cumulative - (buckets[1]+buckets[2]+lastCall))
           ELSE IF message /\ record \notin seen THEN delta ELSE 0
     IN /\ total' = IF record \in {"codex3", "codex5"} THEN cumulative
                    ELSE total + contribution
        /\ buckets' = [buckets EXCEPT ![model] = @ + contribution]
        /\ seen' = IF message THEN seen \cup {record} ELSE seen
        /\ model' = IF record = "switch" THEN 2 ELSE model
        /\ turn' = CASE record = "human" -> "open" [] record = "abort" -> "aborted"
                     [] record = "rollback" -> "rolledBack"
                     [] record = "complete" \/ message -> "complete"
                     [] OTHER -> turn
  /\ cursor' = cursor + 1 /\ UNCHANGED <<provider, sealed, partial>>
Rescan ==
  /\ cursor > 0 /\ cursor' = 0 /\ total' = 0 /\ buckets' = <<0,0>>
  /\ seen' = {} /\ model' = 1 /\ turn' = "none"
  /\ UNCHANGED <<provider, sealed, partial>>
Next == BeginLine \/ Newline \/ Parse \/ Rescan
Spec == Init /\ [][Next]_vars

\* Independently specified prefix oracles for all three fixtures, including
\* the interrupted/replayed prefixes, both model buckets and turn lifecycle.
FirstCharge == IF provider = "codex" THEN 3 ELSE 1
SecondLine == IF provider = "claude" THEN 9 ELSE 8
ExpectedBuckets ==
  <<IF cursor >= 3 THEN FirstCharge ELSE 0, IF cursor >= SecondLine THEN 2 ELSE 0>>
ExpectedTurn ==
  CASE cursor < 2 -> "none"
    [] provider = "codex" /\ cursor = 9 -> "aborted"
    [] provider = "codex" /\ cursor = 10 -> "rolledBack"
    [] provider = "codex" /\ cursor = 12 -> "complete"
    [] provider \in {"claude","gemini"} /\ cursor >= 3 -> "complete"
    [] OTHER -> "open"
TypeOK ==
  /\ provider \in Providers /\ partial \in BOOLEAN
  /\ cursor \in 0..sealed /\ sealed \in 0..Len(Records(provider))
  /\ total \in Nat /\ buckets \in [1..2 -> Nat] /\ model \in 1..2
  /\ seen \subseteq {"msg1","msg2"}
PrefixAccounting == buckets = ExpectedBuckets /\ total = buckets[1]+buckets[2]
Lifecycle == turn = ExpectedTurn
=============================================================================
