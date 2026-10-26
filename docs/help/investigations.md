# Investigations

Saved investigations survive restart and job cleanup. Search finds a saved
investigation by literal text in its question or specialist ID, including older
saved investigation versions that are not currently loaded in the list. Matching
ignores ASCII letter case; other characters compare exactly. Search results are
shown newest first and can be paged with **Load older investigations**. Leading
and trailing ASCII whitespace is trimmed; the raw query is bounded before
trimming.

Search covers saved summary metadata only. It does not search activity entries,
findings, or agent-chat messages. Clearing the search restores the ordinary
saved-investigation list. Opening an older investigation preserves the version
and basis that were originally saved.

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
