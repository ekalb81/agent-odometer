---------------------------- MODULE ScanCache ------------------------------
EXTENDS Naturals

\* One path and one disposable cache row. Generation 1 is read at startup.
\* A subsequent change advances at least one OS stamp component. Byte changes
\* invisible to size/mtime/file identity are outside the protocol's guarantee.
Faults == {"none","size","mtime","identity","legacy","parseVersion","newApp","readError","adapter","absent"}
CurrentApp == 2
StoredApp(f) == IF f = "newApp" THEN 1 ELSE 2
ParserMeaning == 6
\* Version 5 can decode successfully yet contains the old token attribution.
ParseVersion == 6
StoredParseVersion(f) == IF f = "parseVersion" THEN 5 ELSE 6
StoredMeaning(f) == IF f = "parseVersion" THEN 5 ELSE ParserMeaning
Base == [size |-> 1,mtime |-> 1,id |-> 1]
Stamp(n) == IF n = 1 THEN Base ELSE [Base EXCEPT !.mtime = 2]
StoredStamp(f) == CASE f = "size" -> [Base EXCEPT !.size = 0]
                    [] f = "mtime" -> [Base EXCEPT !.mtime = 0]
                    [] f \in {"identity","legacy"} -> [Base EXCEPT !.id = 0]
                    [] OTHER -> Base
No == [via |-> "none",value |-> 0,at |-> 0,started |-> 0]
VARIABLES fault, disk, stage, started, data, result
vars == <<fault,disk,stage,started,data,result>>
Init ==
  /\ fault \in Faults /\ disk = 1 /\ stage = "start" /\ started = 0 /\ data = 0 /\ result = No
BeforeLookup ==
  /\ stage = "start" /\ started' = disk /\ stage' = "lookup"
  /\ UNCHANGED <<fault,disk,data,result>>
Lookup ==
  /\ stage = "lookup"
  /\ LET enabled == fault # "absent" /\ StoredParseVersion(fault) = ParseVersion
         decoded == fault # "readError"
         sameFile == StoredStamp(fault) = Stamp(started) /\ Stamp(disk) = Stamp(started)
         hit == enabled /\ decoded /\ sameFile /\ fault # "adapter"
     IN /\ stage' = IF hit THEN "cached" ELSE "parse"
        /\ data' = IF hit THEN (IF StoredMeaning(fault) = ParserMeaning THEN 1 ELSE 0) ELSE 0
  /\ UNCHANGED <<fault,disk,started,result>>
FreshParse(ok) ==
  /\ stage = "parse" /\ started' = disk
  /\ stage' = IF ok THEN "parsed" ELSE "failed"
  /\ data' = IF ok THEN disk ELSE 0
  /\ UNCHANGED <<fault,disk,result>>
Publish ==
  /\ stage \in {"cached","parsed"}
  /\ stage' = IF started = disk THEN "done" ELSE "failed"
  /\ result' = IF started = disk THEN [via |-> stage,value |-> data,at |-> disk,started |-> started]
               ELSE No
  /\ UNCHANGED <<fault,disk,started,data>>
Change == /\ disk = 1 /\ disk' = 2 /\ UNCHANGED <<fault,stage,started,data,result>>
Next == BeforeLookup \/ Lookup \/ (\E b \in BOOLEAN: FreshParse(b)) \/ Publish \/ Change
        \/ (stage \in {"failed","done"} /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars
PublishedMatchesFreshSource ==
  /\ (stage = "done" => result.value = result.at /\ result.started = result.at)
  /\ (result.via = "cached" => fault \in {"none","newApp"} /\ result.at = 1)
  /\ (stage = "failed" => result = No)
Failover ==
  [] [(stage = "lookup" /\ stage' # "lookup" /\ (fault \notin {"none","newApp"} \/ disk # started \/ started # 1)
       => stage' = "parse")]_vars
HealthyHit ==
  [] [(stage = "lookup" /\ stage' # "lookup" /\ fault \in {"none","newApp"} /\ started = 1 /\ disk = 1
       => stage' = "cached")]_vars
=============================================================================
