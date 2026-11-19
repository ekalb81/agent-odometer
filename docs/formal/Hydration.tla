---------------------------- MODULE Hydration ------------------------------
EXTENDS Naturals, StartupGate

\* Capture follows listener registration. A callback can deliver an update or
\* removal before or after either list reply. For one key the last event wins.
Values == {"old","new","absent"}
None == "noEvent"
VARIABLES source, store, buffered, issued, outstanding, captured, gate, proof,
          terminal, exposed, updates, listening
vars == <<source,store,buffered,issued,outstanding,captured,gate,proof,terminal,exposed,updates,listening>>
Init ==
  /\ source = "old" /\ store = "absent" /\ buffered = None /\ issued = 0
  /\ outstanding = {} /\ captured = <<None,None>> /\ gate = PendingGate /\ proof = FALSE /\ listening = FALSE
  /\ terminal = FALSE /\ exposed = FALSE /\ updates = 0
Listen ==
  /\ ~listening /\ listening' = TRUE
  /\ UNCHANGED <<source,store,buffered,issued,outstanding,captured,gate,proof,terminal,exposed,updates>>
List ==
  /\ listening /\ issued < 2 /\ ~exposed /\ issued' = issued+1
  /\ outstanding' = outstanding \cup {issued'} /\ captured' = [captured EXCEPT ![issued'] = source]
  /\ gate' = PendingGate /\ proof' = FALSE
  /\ UNCHANGED <<source,store,buffered,terminal,exposed,updates,listening>>
Mutation(v) ==
  /\ updates < 2 /\ source' = v /\ updates' = updates+1
  /\ IF gate.verified THEN /\ store' = v /\ buffered' = None
              ELSE /\ store' = store /\ buffered' = IF listening THEN v ELSE None
  /\ UNCHANGED <<issued,outstanding,captured,gate,proof,terminal,exposed,listening>>
Reply(id,ok) ==
  /\ id \in outstanding /\ outstanding' = outstanding \ {id}
  /\ IF id = issued THEN
        /\ gate' = ReplyGate(ok) /\ proof' = ok
        /\ store' = IF ok THEN (IF buffered # None THEN buffered ELSE captured[id]) ELSE store
        /\ buffered' = IF ok THEN None ELSE buffered
        /\ exposed' = Exposable(terminal,gate')
     ELSE UNCHANGED <<store,buffered,gate,proof,exposed>>
  /\ UNCHANGED <<source,issued,captured,terminal,updates,listening>>
ScanComplete ==
  /\ ~terminal /\ terminal' = TRUE /\ exposed' = Exposable(TRUE,gate)
  /\ UNCHANGED <<source,store,buffered,issued,outstanding,captured,gate,proof,updates,listening>>
Next == Listen \/ List \/ (\E v \in Values: Mutation(v)) \/ (\E i \in 1..2,b \in BOOLEAN: Reply(i,b))
        \/ ScanComplete \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
CompleteGate == exposed => proof /\ gate.verified /\ terminal /\ buffered = None
LatestVerifiedSnapshot == proof => gate.verified /\ store = source
IgnoreSuperseded ==
  [] [((outstanding' # outstanding /\ issued' = issued /\ issued \in outstanding')
       => UNCHANGED <<gate,store,proof,buffered,exposed>>)]_vars
=============================================================================
