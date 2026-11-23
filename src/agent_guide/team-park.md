Park a task on an outside event (hgs guide team-park)

Never wait in a loop: no sleep, no polling a version, a CI page or the
inbox. When your task can go on only after something outside it happens,
park it with the condition and your estimate, then end your turn:
  {"team":"ID","task":N,"attempt":"ATTEMPT","condition":{...},
   "eta_minutes":40,"note":"what is done, what is left"} |
    hgs team park --json

A condition is one of:
  {"kind":"ci","repo":"OWNER/REPO","run":RUN_ID}     the run finishes
  {"kind":"build","version":"1.58.0"}                that version installed
    ("build":N for a Dev number, "commit":"SHA"; "program":"ABSOLUTE PATH"
    of a desktop that answers --build-info)
  {"kind":"task","task":N}                           task N is accepted
  {"kind":"card","proposal":"pN"}                    the owner answers card pN
  {"kind":"time","at":UNIX_SECONDS}                  that time comes
  {"kind":"run","request":"RUN_REQUEST_ID"}          your own hgs team run
    finishes (whatever its exit code; the task comes back with it)

The attempt is kept; you are free at once and get your next task. Zerus
checks the condition itself (bounded) and gives the task back to you at
your next free turn, in the same attempt, with your note. If it fails (the
run fails, the card is denied) the lead is told why and decides.

Status and your session show "parked until <condition>, ETA <time>", never
Building. Done anyway? Report on the parked attempt as usual. The lead (or
you) gives it back early: {"team":"ID","task":N,"reason":"..."} |
hgs team unpark --json. The lead also parks a member's task.

Zerus enforces it. hgs run refuses a job that waits (named a wait, or a
sleep or ping loop around a check), naming this park. A turn that ends
waiting gets the park offered. An attempt that runs a waiting job or
repeats the same read-only check at intervals is parked by Zerus on a
guessed condition; the lead is told once and sets the real one.

Lead: waiting is always a park (hgs guide team-lead); a reply or message telling a member
to wait is refused. You get every park's outcome: met, failed with the reason, or overdue.
