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
