# Agent Chats

Review saved specialist conversations and open their findings. The list uses
the same saved investigation history as Investigations, including its search,
active filter, and **Load older saved tasks** continuation. Each docked task
shows its saved question, producer identity, and typed result reference when
those records are available. Older tasks without a persisted conversation
remain identifiable and can still be opened in Investigations.

Choose **Open chat** on a saved-result notice to dock that task's validated
persisted conversation in Agent Chats. Choose **Open findings** to open the
matching task in Investigations. Viewing a transcript keeps its saved-result
notice unread; only showing the matching findings consumes that notice. These
actions do not start work or change the composer or an uncertain start request.
The notice is not a message-read indicator, and this surface does not provide
freeform assistant messages or streaming.

Select history entries to dock up to four real saved task chats. Reopening an
existing chat focuses and refreshes it without changing its docking age. A fifth
successfully loaded chat replaces the oldest dock; failed and legacy reads do
not replace a pane. Close removes only the pane, so history can reopen it.

For a supported saved task with a validated result, choose **Follow-up task**
to prepare a new child investigation from that exact task. The dialog shows its
saved scope, specialist version, provider mode and producer attribution; the
current configured provider for that mode is used. Completion roles require
fresh recorded inventory choices. The follow-up is a separate task and saved
chat linked to the parent, not a message appended to the parent transcript.
Only a confirmed child opens in Agent Chats; an uncertain start retains the
same request and nonce for retry. Preparation or admission failures never
substitute another parent or specialist. This increment does not provide
free-form replies or streaming.

Panes retain last-observed snapshots, including pending chats; they do not imply
live updates. Use **Focus / refresh** to reread a pane. Layout adjusts to one
through four panes and is session-only. These are individual task chats, not
merged multi-task conversation threads.
