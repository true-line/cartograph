---
"cartograph": minor
---

A scheduled Python job such as `python cron.py -j stats` now enters the function its argument selects when the script's `__main__` argparse dispatch proves it, so each job's flow starts at its own code instead of the whole file (#779).
