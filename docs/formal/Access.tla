------------------------------ MODULE Access -------------------------------
EXTENDS Naturals

\* Explicit local transcript page, anchored record, and retained search target.
\* A stable record survives append; a page continuation binds the entire file
\* generation. Retained search uses a materialized snapshot revision and does
\* not need the original file. Keys are opaque; callers cannot supply paths.
Kinds == {"page","record","retained"}
Sources == {"stable","missing","unobserved","outside","ambiguous"}
VARIABLES op, owner, disk, stamp, anchor, revision, registered, request, receipt
vars == <<op,owner,disk,stamp,anchor,revision,registered,request,receipt>>
Empty == [owner |-> 0, key |-> 0]
Key(k) == CASE k = "page" -> stamp [] k = "record" -> anchor [] OTHER -> revision
Token(k) == [owner |-> owner,key |-> Key(k)]
Eligible == owner > 0 /\ IF op = "retained" THEN disk # "ambiguous"
                         ELSE disk = "stable" /\ registered = stamp
Init ==
  /\ op \in Kinds /\ owner = 1 /\ disk = "stable"
  /\ stamp = 1 /\ anchor = 1 /\ revision = 1 /\ registered = 1
  /\ request = Empty /\ receipt = [ok |-> FALSE,used |-> Empty,actual |-> Empty,
                                   source |-> "missing", observed |-> FALSE]
Capture ==
  /\ Eligible /\ request' = Token(op)
  /\ UNCHANGED <<op,owner,disk,stamp,anchor,revision,registered,receipt>>
Resolve ==
  /\ request # Empty
  /\ receipt' = [ok |-> (Eligible /\ request = Token(op)),used |-> request,
                 actual |-> Token(op),source |-> disk,observed |-> registered = stamp]
  /\ UNCHANGED <<op,owner,disk,stamp,anchor,revision,registered,request>>
ChangeFile(changeAnchor) ==
  /\ owner > 0 /\ stamp < 3 /\ stamp' = stamp+1 /\ disk' = "unobserved"
  /\ anchor' = IF changeAnchor THEN anchor+1 ELSE anchor
  /\ UNCHANGED <<op,owner,revision,registered,request,receipt>>
Observe ==
  /\ owner > 0 /\ disk = "unobserved" /\ disk' = "stable" /\ registered' = stamp
  /\ revision' = revision+1
  /\ UNCHANGED <<op,owner,stamp,anchor,request,receipt>>
Lose(s) ==
  /\ owner > 0 /\ disk' = s
  /\ UNCHANGED <<op,owner,stamp,anchor,revision,registered,request,receipt>>
Purge ==
  /\ owner = 1 /\ owner' = 0 /\ disk' = "missing"
  /\ UNCHANGED <<op,stamp,anchor,revision,registered,request,receipt>>
Recreate ==
  /\ owner = 0 /\ owner' = 2 /\ disk' = "stable" /\ registered' = stamp
  /\ UNCHANGED <<op,stamp,anchor,revision,request,receipt>>
Next == Capture \/ Resolve \/ (\E a \in BOOLEAN: ChangeFile(a)) \/ Observe
        \/ (\E s \in {"missing","outside","ambiguous"}: Lose(s)) \/ Purge \/ Recreate
        \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
ProofAtRead ==
  /\ receipt.actual.owner # 0 /\ receipt.source # "ambiguous"
  /\ (op # "retained" => receipt.source = "stable" /\ receipt.observed)
VerifiedResults ==
  receipt.ok => ProofAtRead /\ receipt.actual = receipt.used /\ receipt.used.owner # 0
UsableCurrentTarget ==
  (ProofAtRead /\ receipt.used # Empty /\ receipt.used = receipt.actual) => receipt.ok
=============================================================================
