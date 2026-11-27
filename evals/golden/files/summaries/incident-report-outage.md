# Incident report: warehouse order system outage

**Company:** Tallow Logistics
**Incident:** INC-2026-041
**Date:** Wednesday 14 October 2026
**Duration:** 3 hours 12 minutes (06:48–10:00)
**Severity:** High
**Author:** Rui Castellano, Platform team lead

## What happened
At 06:48 the warehouse order system stopped receiving new orders from the web shop. Pickers' handheld scanners showed "no work available". Orders kept arriving in the web shop but were not passed to the warehouse.

## Impact
- 1,840 orders were delayed. 1,610 still went out the same day; 230 shipped a day late.
- 41 customers contacted support. 12 asked for refunds of delivery charges, which were given.
- About 60 picker-hours were lost while the team waited.
- No orders or data were lost.

## Cause
The internal message queue that passes orders from the web shop to the warehouse system uses a security certificate to encrypt traffic. That certificate **expired at 06:45**. After that, the warehouse system refused the connection. The certificate had been set up by hand two years ago and was not covered by our automatic renewal, so nobody got a reminder.

## Timeline
- 06:48 First failed connection; no alert fired, because the queue's health check only checked that the service was running, not that messages were flowing.
- 07:20 Shift manager phones the on-call engineer (Dana Whitlock) after pickers report no work.
- 08:05 Dana finds certificate errors in the logs.
- 09:15 New certificate issued and installed.
- 10:00 Backlog cleared; normal operation.

## What went well
- Shift manager escalated quickly once the problem was visible.
- No data was lost; the web shop kept all orders.

## What went badly
- No alert for 32 minutes; the problem was found by people, not monitoring.
- It took 45 minutes to find the cause because the error message was unclear.

## Follow-up actions
| # | Action | Owner | Due |
|---|---|---|---|
| 1 | Move all certificates, including this one, to automatic renewal | Dana Whitlock | 30 October 2026 |
| 2 | Add an alert if no orders reach the warehouse for 10 minutes during opening hours | Rui Castellano | 6 November 2026 |
| 3 | List every certificate we use and its expiry date; review monthly | Femi Adebayo | 13 November 2026 |
| 4 | Improve the error message on the warehouse side to name the certificate problem | Dana Whitlock | 20 November 2026 |
