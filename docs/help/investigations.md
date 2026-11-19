# Investigations

Saved investigations survive restart and job cleanup. Search finds a saved
investigation by literal text in its question or specialist ID, including older
saved investigation versions that are not currently loaded in the list. Matching
ignores ASCII letter case; other characters compare exactly. Search results are
shown newest first and can be paged with **Load older investigations**. Leading
and trailing ASCII whitespace is trimmed; the raw query is bounded before
trimming.

Search also matches accepted saved finding titles and statements. **Answer match**
labels those hits; opening still loads the exact task and cited findings. Search
does not inspect activity entries, raw responses, source text or arbitrary chat prose. Clearing the search restores the ordinary
saved-investigation list. Opening an older investigation preserves the version
and basis that were originally saved.

Each saved finding has two separate reviews. **Use for follow-up** selects it
for later work and changes nothing else. **Accept as curated** lets the curated
reads show it: a best-effort export, the Spec Workbench and the read-only MCP
list it as a cited statement while every fact it cites is unchanged. The finding
stays T3 / InferredWeak, nothing is written to the graph, and a verified-only
export never shows it. **Reject as curated** withdraws an acceptance. If a
curation review cannot be saved or confirmed, choose **Refresh curation
reviews** and decide again; nothing is retried for you.

From a selected saved task, open **Conversation turns** to browse its persisted
follow-ups. Turns are loaded in bounded newest-first pages; a selected task stays
visible even when it falls outside the current page. Parent links identify the
task each follow-up came from, so sibling follow-ups remain siblings. Selecting
a turn opens only that task's saved activity, findings, and citations. This
view navigates investigation tasks; it does not represent freeform agent chat.

The specialist list also shows each active task's saved question, current status,
and saved provider/model. Choose **Open investigation** on a task row to inspect
that exact saved task. This does not change the selected specialist or your
composer draft; an uncertain start can still be retried with its original request.
Only exact specialist versions with observed tasks get a group; a complete empty
observation shows one **No active tasks** message for the list. During refresh,
last-observed rows are labeled as such. If a refresh fails, current activity is
unknown and retained rows are not presented as current. The shared activity
summary counts queued, preparing, running and awaiting-consent tasks, with
cancellation-requested tasks shown as a subset. Counts from a complete page are
exact for that bounded observation. A continuation page or unmatched specialist
version is labeled as a partial lower bound; it does not claim the full runtime
queue. During refresh the summary is last observed, and an error hides counts
along with stale task rows.

Active task rows also show **Reported tokens so far** when the saved telemetry fits the existing activity projection. Input and output can independently be **Unknown**; omitted metadata is **Unavailable**. These provider reports may be incomplete while newer work is in flight. They are not token reservations, a response-model identity, monetary cost or a global/history bill. The existing partial and last-observed labels still apply.

The app shows a **Saved result ready** notice only after it has first observed a
valid saved question without a result, then later observes that task's saved
result reference. A result already present on the first valid observation is
treated as known and stays silent; a task that finishes before a valid pending
observation may not produce a notice. Legacy, missing, malformed, and failed
reads do not establish pending evidence. Open a notice to load that exact
investigation. Dismissing the toast keeps it in the session-only **New results**
list; the notice is consumed only after the matching saved result and transcript
are visible. This checks persisted task-bound result references only. It does
not recover unread state after restart or create assistant messages.

Active task rows also show **Provider-reported cache-read tokens so far**, independently of input/output reports. Exact zero is shown as zero, **Unknown** means no known report, and **Unavailable** means the optional metadata was omitted. Reports may be incomplete and are not a price or a complete lifetime bill. Refresh keeps last-observed values; an error reports current activity unknown and hides retained rows.
