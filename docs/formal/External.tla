---------------------------- MODULE External -------------------------------
EXTENDS Naturals

\* One external service at a time, selected in Init. Time is measured in due
\* ticks. Real status polls have two fixed unauthenticated HTTPS hosts; live
\* quota uses the explicitly selected Codex account via local CLI stdio.
Services == {"status","quota"}
Providers == {"codex","claude","gemini"}
Accounts == {"A","B"}
No == "none"
VARIABLES service, provider, epoch, target, candidate, grants, flight, reading, status,
          cooldown, used, lastIdentity, ledger, sourceUnits
vars == <<service,provider,epoch,target,candidate,grants,flight,reading,status,cooldown,
          used,lastIdentity,ledger,sourceUnits>>
Supported == IF service = "quota" THEN provider = "codex" ELSE provider # "gemini"
Targets == IF service = "quota" THEN Accounts ELSE {"public"}
Ticket(e,t) == [epoch |-> e,target |-> t]
EmptyTicket == Ticket(0,No)
Tickets == [epoch:0..3,target:Accounts \cup {"public"}]
Init ==
  /\ service \in Services /\ provider \in Providers /\ epoch = 0 /\ target = No
  /\ grants = {} /\ candidate = No /\ flight = EmptyTicket /\ reading = EmptyTicket /\ used = {} /\ lastIdentity = EmptyTicket
  /\ status = "disabled" /\ cooldown = 0 /\ sourceUnits = 0 /\ ledger = 0
Configure(t) ==
  /\ t # target /\ epoch < 3 /\ epoch' = epoch+1 /\ target' = t
  /\ (service = "quota" /\ t # No => Supported /\ (candidate = t \/ \E g \in grants: g.target = t))
  /\ candidate' = No
  /\ grants' = IF t = No THEN grants ELSE grants \cup {Ticket(epoch+1,t)}
  /\ reading' = EmptyTicket /\ status' = IF t = No THEN "disabled" ELSE "pending"
  /\ cooldown' = 0
  /\ UNCHANGED <<service,provider,flight,used,ledger,sourceUnits>>
Identify(t) ==
  /\ service = "quota" /\ Supported /\ flight = EmptyTicket
  /\ candidate' = t \* one explicit, user-authorized identity RPC; No represents failure
  /\ lastIdentity' = Ticket(epoch,t)
  /\ UNCHANGED <<service,provider,epoch,target,grants,flight,reading,status,cooldown,used,ledger,sourceUnits>>
Start ==
  /\ Supported /\ target # No /\ flight = EmptyTicket /\ cooldown = 0
  /\ flight' = Ticket(epoch,target) /\ used' = used \cup {flight'}
  /\ UNCHANGED <<service,provider,epoch,target,candidate,grants,reading,status,cooldown,ledger,sourceUnits>>
Finish(returned,success) ==
  /\ flight # EmptyTicket /\ flight' = EmptyTicket
  /\ LET active == target # No /\ flight = Ticket(epoch,target)
         match == success /\ returned = flight.target
     IN /\ reading' = IF active THEN (IF match THEN Ticket(flight.epoch,returned) ELSE EmptyTicket) ELSE reading
        /\ status' = IF active THEN (IF match THEN "current" ELSE "unavailable") ELSE status
        /\ cooldown' = IF active THEN (IF match THEN 1 ELSE 2) ELSE cooldown
  /\ UNCHANGED <<service,provider,epoch,target,candidate,grants,used,ledger,sourceUnits>>
Tick ==
  /\ cooldown > 0 /\ cooldown' = cooldown-1
  /\ UNCHANGED <<service,provider,epoch,target,candidate,grants,flight,reading,status,used,ledger,sourceUnits>>
Expire ==
  /\ status = "current" /\ status' = "stale"
  /\ UNCHANGED <<service,provider,epoch,target,candidate,grants,flight,reading,cooldown,used,ledger,sourceUnits>>
LocalUsage ==
  /\ sourceUnits < 1 /\ sourceUnits' = sourceUnits+1 /\ ledger' = ledger+1
  /\ UNCHANGED <<service,provider,epoch,target,candidate,grants,flight,reading,status,cooldown,used>>
Next == \/ ((\E t \in Targets \cup {No}: Configure(t)) \/ Start
             \/ (\E r \in Targets, ok \in BOOLEAN: Finish(r,ok)) \/ Tick \/ Expire \/ LocalUsage)
            /\ UNCHANGED lastIdentity
        \/ \E t \in Accounts \cup {No}: Identify(t)
        \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
TypeOK ==
  /\ service \in Services /\ provider \in Providers /\ epoch \in 0..3
  /\ target \in Targets \cup {No} /\ candidate \in Accounts \cup {No}
  /\ flight \in Tickets \cup {EmptyTicket}
  /\ reading \in Tickets \cup {EmptyTicket} /\ used \subseteq Tickets /\ grants \subseteq Tickets
  /\ status \in {"disabled","pending","current","unavailable","stale"}
  /\ cooldown \in 0..2 /\ sourceUnits \in 0..1 /\ ledger \in 0..1
Authorized == used \subseteq grants /\ (~Supported => used = {})
IdentityRequests ==
  [] [(lastIdentity' # lastIdentity => service = "quota" /\ Supported /\ flight = EmptyTicket
       /\ lastIdentity' = Ticket(epoch,candidate')
       /\ UNCHANGED <<grants,target,reading,used,ledger>>)]_vars
CurrentHasEvidence ==
  /\ (status = "current" => reading = Ticket(epoch,target) /\ target # No /\ reading \in used)
  /\ (target = No => status = "disabled" /\ reading = EmptyTicket)
  /\ (status = "unavailable" => reading = EmptyTicket)
LocalAccounting == ledger = sourceUnits
PeriodicGate ==
  [] [(flight = EmptyTicket /\ flight' # EmptyTicket =>
       Supported /\ target # No /\ cooldown = 0 /\ flight'.target = target)]_vars
ApprovedCandidate ==
  [] [(service = "quota" /\ target' # target /\ target' # No =>
       target' = candidate \/ (\E g \in grants: g.target = target'))]_vars
=============================================================================
