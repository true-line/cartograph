# Expected entry points: SimpleLogin (#1752)

The held-out list for question 17 of the SPEC-16 checklist: every way work
reaches the system, written by hand from the source of `simple-login/app`
at `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`. Keep this file outside every
model input, and away from the reviewer until the rest of the checklist is
answered.

It was written on 2026-11-20 by the reviewer of review 1, after that review's
checklist was answered and from the source alone. For a later review it is a
list made before the run.

| # | Entry point | What starts it | Source |
|---|---|---|---|
| E1 | Web and API server | An HTTP request | `wsgi.py`, `server.py`; `Dockerfile` `CMD gunicorn wsgi:app` |
| E2 | Mail handler | A message delivered over SMTP (port 20381 by default): mail to an alias is forwarded, a reply is sent on, a bounce is handled | `email_handler.py:2499-2507` |
| E3 | Scheduled jobs | 18 yacron entries, each `cron.py -j <job>` | `crontab.yml`, `crontab-all-hosts.yml`, `cron.py` |
| E4 | Job runner | A row in the job table that is due; the process polls in a loop | `job_runner.py:433-440` |
| E5 | Event listener | A database notification (`LISTEN`), or the dead-letter sweep | `event_listener.py:98-110`, `events/event_source.py:47` |
| E6 | Monitoring loop | A timer in a process of its own; exports metrics | `monitoring.py:192-204` |
| E7 | Operator commands | A person runs one of 17 scripts | `commands/*.py` |
| E8 | Operator shell | A person opens it | `shell.py` |

Webhooks (Paddle, Apple, Twilio, Coinbase) arrive as HTTP requests and are
part of E1. The browser extension and the Android app are clients of E1 and
have their own entry points (commands, screens), which this list does not
cover.
