--------------------------- MODULE StartupGate ------------------------------
\* Listeners are installed before the list is captured. A rejected list may
\* finish startup UI work, but cannot certify a complete accounting surface.
ReplyGate(ok) == [settled |-> TRUE, verified |-> ok]
PendingGate == [settled |-> FALSE, verified |-> FALSE]
Exposable(scanDone,gate) == scanDone /\ gate.verified
=============================================================================
