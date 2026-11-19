---------------------------- MODULE Odometer -------------------------------
EXTENDS Naturals, StartupGate

\* Cross-layer slice: one session, up to two completed usage records charged
\* at one and two units. The UI asks for project A, either all time or the
\* second time window. Reassignment to B races a request. Only a prepared,
\* complete-coverage aggregate is modeled at the display boundary.
VARIABLES listResult, disk, exists, resident, facts, history, scanned, gate,
          project, window, card, request, view
vars == <<listResult,disk,exists,resident,facts,history,scanned,gate,project,window,card,request,view>>
No == [facts |-> 0,project |-> "none",window |-> "none",card |-> 0,raw |-> 0,price |-> 0]
Ready == Exposable(scanned,gate) /\ history = "complete"
Total(n,w) == CASE n = 2 -> IF w = "all" THEN 3 ELSE 2
                [] n = 1 /\ w = "all" -> 1 [] OTHER -> 0
Frame ==
  LET n == IF project = "A" THEN Total(facts,window) ELSE 0
  IN [facts |-> facts,project |-> project,window |-> window,card |-> card,raw |-> n,price |-> n*card]
Same(r) == r.facts = facts /\ r.project = project /\ r.window = window /\ r.card = card
Init ==
  /\ disk = 0 /\ exists = TRUE /\ resident = 0 /\ facts = 0 /\ history = "pending"
  /\ scanned = FALSE /\ gate = PendingGate /\ listResult = "pending" /\ project = "A" /\ window = "all"
  /\ card = 1 /\ request = No /\ view = No
Append ==
  /\ exists /\ disk < 2 /\ disk' = disk+1
  /\ UNCHANGED <<exists,resident,facts,history,scanned,gate,project,window,card,request,view>>
HistoryOpen(h) ==
  /\ history = "pending" /\ history' = h
  /\ UNCHANGED <<disk,exists,resident,facts,scanned,gate,project,window,card,request,view>>
Observe ==
  /\ exists /\ resident < disk /\ history # "pending" /\ resident' = disk
  /\ facts' = IF history = "unavailable" THEN facts ELSE disk
  /\ view' = IF facts' # facts THEN No ELSE view
  /\ UNCHANGED <<disk,exists,history,scanned,gate,project,window,card,request>>
Remove ==
  /\ exists /\ exists' = FALSE
  /\ UNCHANGED <<disk,resident,facts,history,scanned,gate,project,window,card,request,view>>
FinishScan ==
  /\ ~scanned /\ (~exists \/ resident = disk) /\ scanned' = TRUE
  /\ UNCHANGED <<disk,exists,resident,facts,history,gate,project,window,card,request,view>>
ListReply(ok) ==
  /\ ~gate.verified
  /\ gate' = ReplyGate(ok)
  /\ listResult' = IF ok THEN IF listResult = "failed" THEN "recovered" ELSE "success"
                   ELSE "failed"
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,project,window,card,request,view>>
ChangeProject ==
  /\ project = "A" /\ project' = "B" /\ view' = No
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,gate,window,card,request>>
ChangeWindow ==
  /\ window = "all" /\ window' = "last" /\ view' = No
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,gate,project,card,request>>
ChangeCard ==
  /\ card = 1 /\ card' = 2 /\ view' = No
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,gate,project,window,request>>
Query ==
  /\ Ready /\ request = No /\ request' = Frame
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,gate,project,window,card,view>>
Reply(ok) ==
  /\ request # No /\ request' = No
  /\ view' = IF Ready /\ Same(request) THEN (IF ok THEN request ELSE No) ELSE view
  /\ UNCHANGED <<disk,exists,resident,facts,history,scanned,gate,project,window,card>>
Next == \/ (Append \/ (\E h \in {"complete","partial","unavailable"}: HistoryOpen(h))
             \/ Observe \/ Remove \/ FinishScan \/ ChangeProject \/ ChangeWindow
             \/ ChangeCard \/ Query \/ (\E ok \in BOOLEAN: Reply(ok))) /\ UNCHANGED listResult
        \/ \E ok \in BOOLEAN: ListReply(ok)
        \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars

QueryRequiresSnapshot == (request # No \/ view # No) => listResult \in {"success","recovered"}
VisibleAuthority ==
  view # No =>
    /\ Ready /\ Same(view)
    /\ LET first == IF facts > 0 /\ project = "A" /\ window = "all" THEN 1 ELSE 0
           second == IF facts = 2 /\ project = "A" THEN 2 ELSE 0
       IN /\ view.raw = first+second /\ view.price = (first+second)*card
DurableAuthority ==
  /\ facts <= resident /\ resident <= disk
  /\ (history \in {"complete","partial"} => facts = resident)
  /\ (history \in {"unavailable","pending"} => facts = 0)
NoLossOnRemoval == [][(exists /\ ~exists' => facts' = facts)]_vars
=============================================================================
